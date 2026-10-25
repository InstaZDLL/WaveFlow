// The WebKitGTK bench: drives the release binary through WebKit's remote
// inspector, skin by skin, and reads what each scenario cost from /proc.
// See README.md for what it measures and how to run it.
//
//   bun bench.ts --renderer gpu|software --out <dir>
//                [--skins fluid,liquid,...] [--soak <minutes per skin>]
//                [--effects full|reduced]
//                [--bin <path>] [--sandbox <dir>] [--library <dir>] [--port 9222]
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import sharp from "sharp";
import { Inspector } from "./inspector";
import { byRole, sampleTree, TICKS_PER_SECOND, type TreeSample } from "./proc";

const HERE = dirname(new URL(import.meta.url).pathname);
const REPO = join(HERE, "..", "..", "..");
const HOME = process.env.HOME!;

function arg(name: string, fallback: string): string {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : fallback;
}
const RENDERER = arg("renderer", "gpu");
const OUT = arg(
  "out",
  join(HOME, ".wf-bench", "runs", `${RENDERER}-${Date.now()}`),
);
const BIN = arg(
  "bin",
  join(REPO, "src-tauri", "target", "release", "waveflow"),
);
const SANDBOX = arg("sandbox", join(HOME, ".wf-bench", "sandbox"));
const LIBRARY = arg("library", join(HOME, ".wf-bench", "library"));
const PORT = arg("port", "9222");
const SKINS = arg("skins", "studio,fluid,liquid,pulse,editorial,lounge").split(
  ",",
);
const SOAK_MIN = Number(arg("soak", "0"));
// The interface's effects level, stamped over whatever the app resolved
// (`data-effects`; Automatic reduces under the software renderer). Empty:
// left as the app decided.
const EFFECTS = arg("effects", "");
// However the run ends, an error included, the app does not stay open on
// the desktop.
process.on("exit", () => {
  Bun.spawnSync(["bash", join(HERE, "run-app.sh"), "stop"]);
});
mkdirSync(join(OUT, "shots"), { recursive: true });

const sh = (cmd: string[]) => {
  const r = Bun.spawnSync(cmd, { stdout: "pipe", stderr: "pipe" });
  if (r.exitCode !== 0)
    throw new Error(`${cmd.join(" ")}: ${r.stderr.toString()}`);
  return r.stdout.toString().trim();
};

// ── The app ──────────────────────────────────────────────────────────
async function launch(): Promise<{ pid: number; page: Inspector }> {
  const pid = Number(
    sh(["bash", join(HERE, "run-app.sh"), BIN, SANDBOX, RENDERER, PORT])
      .split("\n")
      .pop(),
  );
  let page: Inspector | null = null;
  for (let i = 0; i < 40 && !page; i++) {
    try {
      const p = new Inspector();
      await p.connect(await Inspector.socketFor(`127.0.0.1:${PORT}`));
      await p.value("document.readyState");
      page = p;
    } catch {
      await Bun.sleep(500);
    }
  }
  if (!page) throw new Error("no inspector");
  await waitFor(page, "!!document.querySelector('footer')", 30_000);
  await page.value(await Bun.file(join(HERE, "page.js")).text());
  return { pid, page };
}

async function waitFor(page: Inspector, cond: string, ms: number) {
  const start = Date.now();
  while (Date.now() - start < ms) {
    try {
      if (await page.value(cond)) return;
    } catch {}
    await Bun.sleep(300);
  }
  throw new Error(`timed out waiting for ${cond}`);
}

/** First run in a sandbox: offline, no onboarding, English, the fixture
 *  library scanned and analysed. Returns whether it reloaded the page. */
async function setUp(page: Inspector): Promise<boolean> {
  const libs = await page.run("window.__wfb.invoke('list_libraries')");
  if (libs.length > 0) {
    // The views are reached by their English labels, whatever the
    // sandbox was last left in.
    if (
      (await page.value("localStorage.getItem('waveflow-language')")) === "en"
    )
      return false;
    await page.value(
      "localStorage.setItem('waveflow-language', 'en'); location.reload(); 0",
    );
    return true;
  }
  await page.run("window.__wfb.invoke('set_offline_mode', { enabled: true })");
  await page.run(
    "window.__wfb.invoke('set_profile_setting', { key: 'onboarding.dismissed', value: 'true', valueType: 'bool', expectedProfileId: null })",
  );
  const lib = await page.run(
    "window.__wfb.invoke('create_library', { input: { name: 'Bench' } })",
  );
  const folderId = await page.run(
    `window.__wfb.invoke('add_folder_to_library', { libraryId: ${lib.id}, path: ${JSON.stringify(LIBRARY)} })`,
  );
  const scan = await page.run(
    `window.__wfb.invoke('scan_folder', { folderId: ${folderId}, deep: false })`,
    900_000,
  );
  console.log("scanned", JSON.stringify(scan));
  await page.value(
    "localStorage.setItem('waveflow-language', 'en'); location.reload(); 0",
  );
  return true;
}

/** Analyse the library (tempo, loudness — Mood Radio's inputs) before
 *  measuring, so the moods have tracks and no analysis runs under the
 *  measurements. A new library is not analysed on its own. */
async function analyse(page: Inspector) {
  const c = await page.run("window.__wfb.invoke('mood_radio_counts')");
  if (c.analysed_tracks >= c.total_tracks) return;
  const t = Date.now();
  const r = await page.run(
    "window.__wfb.invoke('analyze_library')",
    30 * 60_000,
  );
  console.log(
    `analysed in ${((Date.now() - t) / 1000).toFixed(0)} s`,
    JSON.stringify(r),
  );
}

// ── Measurement ──────────────────────────────────────────────────────
interface Cost {
  seconds: number;
  cpuUiPct: number;
  cpuWebPct: number;
  gpuBusyPct: number;
  pssUiMiB: number;
  pssWebMiB: number;
  gpuResidentMiB: number;
  deltaPssWebMiB: number;
  deltaGpuMiB: number;
}

function cost(root: number, a: TreeSample, b: TreeSample): Cost {
  const ra = byRole(a, root);
  const rb = byRole(b, root);
  const s = (b.t - a.t) / 1000;
  const z = {
    pssKiB: 0,
    rssKiB: 0,
    cpuTicks: 0,
    gpuResidentKiB: 0,
    gpuRenderNs: 0,
  };
  const ua = ra.ui ?? z,
    ub = rb.ui ?? z,
    wa = ra.web ?? z,
    wb = rb.web ?? z;
  const gpuA = (ra.ui?.gpuResidentKiB ?? 0) + (ra.web?.gpuResidentKiB ?? 0);
  const gpuB = (rb.ui?.gpuResidentKiB ?? 0) + (rb.web?.gpuResidentKiB ?? 0);
  // Floored at zero: a DRM client closed between the two samples takes
  // its counter with it, which would read as negative GPU time.
  const renderNs = Math.max(
    0,
    ub.gpuRenderNs + wb.gpuRenderNs - (ua.gpuRenderNs + wa.gpuRenderNs),
  );
  const mib = (k: number) => +(k / 1024).toFixed(1);
  return {
    seconds: +s.toFixed(1),
    cpuUiPct: +(
      (100 * (ub.cpuTicks - ua.cpuTicks)) /
      TICKS_PER_SECOND /
      s
    ).toFixed(1),
    cpuWebPct: +(
      (100 * (wb.cpuTicks - wa.cpuTicks)) /
      TICKS_PER_SECOND /
      s
    ).toFixed(1),
    gpuBusyPct: +((100 * renderNs) / 1e9 / s).toFixed(1),
    pssUiMiB: mib(ub.pssKiB),
    pssWebMiB: mib(wb.pssKiB),
    gpuResidentMiB: mib(gpuB),
    deltaPssWebMiB: mib(wb.pssKiB - wa.pssKiB),
    deltaGpuMiB: mib(gpuB - gpuA),
  };
}

/** A capture that is one flat colour is a view that did not paint.
 *  Decoded with `sharp`, a declared dev dependency of the app. */
async function blank(
  png: Buffer,
): Promise<{ blank: boolean; distinct: number }> {
  const { data, info } = await sharp(png)
    .removeAlpha()
    .raw()
    .toBuffer({ resolveWithObject: true });
  const seen = new Set<number>();
  const pixels = info.width * info.height;
  const step = Math.max(1, Math.floor(pixels / 4000));
  for (let i = 0; i < pixels; i += step) {
    const o = i * info.channels;
    seen.add(
      ((data[o] >> 3) << 10) | ((data[o + 1] >> 3) << 5) | (data[o + 2] >> 3),
    );
  }
  return { blank: seen.size <= 2, distinct: seen.size };
}

async function shot(page: Inspector, name: string) {
  const png = await page.screenshot();
  writeFileSync(join(OUT, "shots", `${name}.png`), png);
  return blank(png);
}

async function measure<T extends object>(
  root: number,
  page: Inspector,
  name: string,
  fn: () => Promise<T>,
) {
  const a = sampleTree(root);
  const frames = await fn();
  const b = sampleTree(root);
  const state = await page.run("window.__wfb.state()");
  const picture = await shot(page, name);
  const r = { scenario: name, frames, cost: cost(root, a, b), state, picture };
  console.log(
    name,
    JSON.stringify({ ...frames, ...r.cost, blank: picture.blank }),
  );
  return r;
}

// ── Run ──────────────────────────────────────────────────────────────
const t0 = Date.now();
let { pid, page } = await launch();
if (await setUp(page)) {
  await Bun.sleep(2500);
  page.close();
  page = new Inspector();
  await page.connect(await Inspector.socketFor(`127.0.0.1:${PORT}`));
  await waitFor(page, "!!document.querySelector('footer')", 30_000);
  await page.value(await Bun.file(join(HERE, "page.js")).text());
}
await analyse(page);
if (EFFECTS)
  await page.run(`window.__wfb.setEffects(${JSON.stringify(EFFECTS)})`);
const env = {
  renderer: RENDERER,
  state: await page.run("window.__wfb.state()"),
  webkit: sh([
    "bash",
    "-c",
    "rpm -q webkit2gtk4.1 2>/dev/null || dpkg-query -W libwebkit2gtk-4.1-0 2>/dev/null || true",
  ]),
  kernel: sh(["uname", "-r"]),
  gpu: sh(["bash", "-c", "lspci | grep -iE 'vga|3d|display' | head -2"]),
  session: sh([
    "bash",
    "-c",
    "loginctl show-session $(loginctl list-sessions --no-legend | awk '$4==\"seat0\"{print $1; exit}') -p Type --value 2>/dev/null || true",
  ]),
  commit: sh(["git", "-C", REPO, "rev-parse", "--short", "HEAD"]),
};
console.log("env", JSON.stringify(env));
const ids: number[] = await page.run("window.__wfb.trackIds(40)");

const results: any[] = [];
const soaks: any[] = [];
for (const skin of SKINS) {
  await page.run(`window.__wfb.setSkin(${JSON.stringify(skin)})`);
  await page.run("window.__wfb.closePanel()");
  await page.run(`window.__wfb.play(${JSON.stringify(ids)})`);
  await page.run("window.__wfb.nav('Home')");
  await Bun.sleep(2500);
  const per: any = { skin };
  per.home = await measure(pid, page, `${skin}-home`, () =>
    page.run("window.__wfb.idle(8000)"),
  );
  await page.run("window.__wfb.openPanel(1)");
  await page.run("window.__wfb.nav('Albums')");
  await Bun.sleep(2000);
  per.albums = await measure(pid, page, `${skin}-albums`, () =>
    page.run("window.__wfb.scroll(6000, 14)"),
  );
  await page.run("window.__wfb.closePanel()");
  await page.run("window.__wfb.nav('Tracks')");
  await Bun.sleep(2000);
  per.tracks = await measure(pid, page, `${skin}-tracks`, () =>
    page.run("window.__wfb.scroll(6000, 14)"),
  );
  per.immersive = await measure(pid, page, `${skin}-immersive`, () =>
    page.run("window.__wfb.immersive(8000)"),
  );
  results.push(per);

  if (SOAK_MIN > 0) {
    const samples: {
      t: number;
      pssWebMiB: number;
      pssUiMiB: number;
      gpuMiB: number;
    }[] = [];
    const end = Date.now() + SOAK_MIN * 60_000;
    const sampler = setInterval(() => {
      const s = byRole(sampleTree(pid), pid);
      samples.push({
        t: Date.now(),
        pssWebMiB: (s.web?.pssKiB ?? 0) / 1024,
        pssUiMiB: (s.ui?.pssKiB ?? 0) / 1024,
        gpuMiB:
          ((s.web?.gpuResidentKiB ?? 0) + (s.ui?.gpuResidentKiB ?? 0)) / 1024,
      });
    }, 1000);
    let loops = 0;
    while (Date.now() < end) {
      for (const view of ["Home", "Albums", "Tracks", "Artists"]) {
        await page.run(`window.__wfb.nav(${JSON.stringify(view)})`);
        await page.run("window.__wfb.scroll(2500, 20)");
      }
      await page.run("window.__wfb.openPanel(1)");
      await page.run("window.__wfb.immersive(3000)");
      await page.run("window.__wfb.closePanel()");
      await page.run(`window.__wfb.setSkin('studio')`);
      await page.run(`window.__wfb.setSkin(${JSON.stringify(skin)})`);
      loops++;
    }
    clearInterval(sampler);
    const slope = (key: "pssWebMiB" | "pssUiMiB" | "gpuMiB") => {
      const pts = samples.filter((p) => p.t - samples[0].t > 60_000);
      if (pts.length < 10) return null;
      const xs = pts.map((p) => (p.t - pts[0].t) / 60_000);
      const ys = pts.map((p) => p[key]);
      const mx = xs.reduce((a, b) => a + b, 0) / xs.length;
      const my = ys.reduce((a, b) => a + b, 0) / ys.length;
      let num = 0,
        den = 0;
      for (let i = 0; i < xs.length; i++) {
        num += (xs[i] - mx) * (ys[i] - my);
        den += (xs[i] - mx) ** 2;
      }
      return +(num / den).toFixed(2);
    };
    const first = samples[0],
      last = samples[samples.length - 1];
    const soak = {
      skin,
      minutes: SOAK_MIN,
      loops,
      start: {
        pssWebMiB: +first.pssWebMiB.toFixed(0),
        pssUiMiB: +first.pssUiMiB.toFixed(0),
        gpuMiB: +first.gpuMiB.toFixed(0),
      },
      end: {
        pssWebMiB: +last.pssWebMiB.toFixed(0),
        pssUiMiB: +last.pssUiMiB.toFixed(0),
        gpuMiB: +last.gpuMiB.toFixed(0),
      },
      peak: {
        pssWebMiB: +Math.max(...samples.map((p) => p.pssWebMiB)).toFixed(0),
        gpuMiB: +Math.max(...samples.map((p) => p.gpuMiB)).toFixed(0),
      },
      slopeMiBPerMin: {
        web: slope("pssWebMiB"),
        ui: slope("pssUiMiB"),
        gpu: slope("gpuMiB"),
      },
    };
    console.log("soak", JSON.stringify(soak));
    soaks.push({ ...soak, samples });
  }
}
await page.run("window.__wfb.invoke('player_pause')").catch(() => {});
const alive = sh([
  "bash",
  "-c",
  `kill -0 ${pid} 2>/dev/null && echo yes || echo no`,
]);

// ── Report ───────────────────────────────────────────────────────────
const report = {
  env,
  minutes: +((Date.now() - t0) / 60000).toFixed(1),
  alive,
  results,
  soaks,
};
writeFileSync(join(OUT, "report.json"), JSON.stringify(report, null, 2));
const rows: string[] = [];
rows.push(`# WebKitGTK bench — ${RENDERER}`, "");
rows.push(
  `WebKitGTK ${env.webkit} · kernel ${env.kernel} · ${env.gpu} · session ${env.session} · ${env.state.vw}×${env.state.vh} @${env.state.dpr} · effects ${env.state.effects ?? "(none)"} · commit ${env.commit}`,
  "",
);
rows.push(
  "| skin | scenario | fps | p50 | p95 | p99 | max | >20 ms | >50 ms | CPU UI | CPU web | GPU busy | PSS web | GPU mem | Δ PSS web | Δ GPU | anim (∞) | blur | backdrop | blank |",
);
rows.push(
  "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |",
);
for (const per of results) {
  for (const k of ["home", "albums", "tracks", "immersive"]) {
    const r = per[k];
    const f = r.frames,
      c = r.cost,
      s = r.state;
    rows.push(
      `| ${per.skin} | ${k} | ${f.fps} | ${f.p50} | ${f.p95} | ${f.p99} | ${f.max} | ${f.over20} | ${f.over50} | ${c.cpuUiPct} % | ${c.cpuWebPct} % | ${c.gpuBusyPct} % | ${c.pssWebMiB} | ${c.gpuResidentMiB} | ${c.deltaPssWebMiB} | ${c.deltaGpuMiB} | ${s.animations} (${s.infinite}) | ${s.filters} | ${s.backdrops} | ${r.picture.blank ? "**yes**" : "no"} |`,
    );
  }
}
if (soaks.length) {
  rows.push(
    "",
    "## Soak",
    "",
    "| skin | min | loops | PSS web start → end (peak) | PSS UI start → end | GPU start → end (peak) | slope web | slope UI | slope GPU |",
  );
  rows.push("| --- | --- | --- | --- | --- | --- | --- | --- | --- |");
  for (const s of soaks) {
    rows.push(
      `| ${s.skin} | ${s.minutes} | ${s.loops} | ${s.start.pssWebMiB} → ${s.end.pssWebMiB} (${s.peak.pssWebMiB}) | ${s.start.pssUiMiB} → ${s.end.pssUiMiB} | ${s.start.gpuMiB} → ${s.end.gpuMiB} (${s.peak.gpuMiB}) | ${s.slopeMiBPerMin.web} | ${s.slopeMiBPerMin.ui} | ${s.slopeMiBPerMin.gpu} |`,
    );
  }
}
rows.push(
  "",
  `App still alive at the end: ${alive}. Run: ${report.minutes} min.`,
);
writeFileSync(join(OUT, "report.md"), rows.join("\n") + "\n");
console.log(rows.join("\n"));
page.close();

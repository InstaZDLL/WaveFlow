// Which piece of a view costs the frames: measure the view at rest, then
// again with one piece switched off by an injected stylesheet, one piece
// at a time. Same launch as the bench (sandbox, inspector, renderer).
//
//   bun bisect.ts --renderer software --skin fluid [--view Home] [--ms 5000]
import { dirname, join } from "node:path";
import { Inspector } from "./inspector";

const HERE = dirname(new URL(import.meta.url).pathname);
const REPO = join(HERE, "..", "..", "..");
const HOME = process.env.HOME!;
const arg = (n: string, d: string) => {
  const i = process.argv.indexOf(`--${n}`);
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : d;
};
const RENDERER = arg("renderer", "software");
const SKIN = arg("skin", "fluid");
const VIEW = arg("view", "Home");
const MS = Number(arg("ms", "5000"));
const PORT = arg("port", "9222");
// However the run ends, an error included, the app does not stay open on
// the desktop.
process.on("exit", () => {
  Bun.spawnSync(["bash", join(HERE, "run-app.sh"), "stop"]);
});

/** One piece per entry, by the selectors that draw it; the last three switch off a whole kind. */
const PIECES: Record<string, string> = {
  "grain (body::after)": "body::after { display: none !important; }",
  "atmosphere pools (body::before)":
    "body::before { display: none !important; }",
  "canvas aura (blurred cover)":
    ".fe-canvas__aura { display: none !important; }",
  "suggestion aura (blurred cover)":
    ".fe-suggest__aura { display: none !important; }",
  "mood waves (drift)":
    ".fe-spectrum__wave svg, .aq-capsule__light svg { animation: none !important; }",
  "player progress":
    "footer .fe-progress, footer .fe-horizon, footer [role=slider] { visibility: hidden !important; }",
  "hero cover shadow":
    ".fe-hero-cover, .fe-tile { box-shadow: none !important; }",
  "every animation":
    "*, *::before, *::after { animation: none !important; transition: none !important; }",
  "every filter":
    "*, *::before, *::after { filter: none !important; backdrop-filter: none !important; -webkit-backdrop-filter: none !important; }",
  "every shadow":
    "*, *::before, *::after { box-shadow: none !important; text-shadow: none !important; }",
};

const r = Bun.spawnSync([
  "bash",
  join(HERE, "run-app.sh"),
  join(REPO, "src-tauri/target/release/waveflow"),
  join(HOME, ".wf-bench/sandbox"),
  RENDERER,
  PORT,
]);
if (r.exitCode !== 0) throw new Error(r.stderr.toString());
const page = new Inspector();
for (let i = 0; ; i++) {
  try {
    await page.connect(await Inspector.socketFor(`127.0.0.1:${PORT}`));
    await page.value("document.readyState");
    break;
  } catch {
    if (i > 40) throw new Error("no inspector");
    await Bun.sleep(500);
  }
}
for (
  let i = 0;
  i < 60 &&
  !(await page.value("!!document.querySelector('footer')").catch(() => false));
  i++
)
  await Bun.sleep(500);
await page.value(await Bun.file(join(HERE, "page.js")).text());
await page.run(`window.__wfb.setSkin(${JSON.stringify(SKIN)})`);
const ids = await page.run("window.__wfb.trackIds(40)");
await page.run(`window.__wfb.play(${JSON.stringify(ids)})`);
await page.run(`window.__wfb.nav(${JSON.stringify(VIEW)})`);
await Bun.sleep(3000);

const inject = (css: string) =>
  page.value(
    `(() => { let s = document.getElementById('wf-bisect'); if (!s) { s = document.createElement('style'); s.id = 'wf-bisect'; document.head.appendChild(s); } s.textContent = ${JSON.stringify(css)}; return 1; })()`,
  );
const run = async (label: string, css: string) => {
  await inject(css);
  await Bun.sleep(1200);
  const f = await page.run(`window.__wfb.idle(${MS})`);
  console.log(`| ${label} | ${f.fps} | ${f.p50} | ${f.p95} | ${f.over20} |`);
  return f;
};
console.log(
  `## ${SKIN} — ${VIEW}, ${RENDERER}\n\n| switched off | fps | p50 | p95 | >20 ms |\n| --- | --- | --- | --- | --- |`,
);
await run("nothing (baseline)", "");
for (const [label, css] of Object.entries(PIECES)) await run(label, css);
await run("nothing (baseline, again)", "");
page.close();

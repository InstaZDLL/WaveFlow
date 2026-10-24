// What the app's processes cost, read from procfs: memory (RSS and PSS
// from `smaps_rollup`), CPU time, and the GPU memory and render-engine
// time the DRM driver reports per open file in `fdinfo` (i915 / xe /
// amdgpu all follow the kernel's `drm-*` keys). WebKitGTK splits the
// app in three: the UI process (the Tauri binary), a `WebKitWebProcess`
// per page and a `WebKitNetworkProcess`; the compositing surfaces #820
// found growing live in the web process, so it is read on its own.
import { readdirSync, readFileSync } from "node:fs";

export interface ProcSample {
  pid: number;
  name: string;
  rssKiB: number;
  pssKiB: number;
  cpuTicks: number;
  gpuResidentKiB: number;
  gpuRenderNs: number;
}

const CLK_TCK = 100;

function read(path: string): string | null {
  try {
    return readFileSync(path, "utf8");
  } catch {
    return null;
  }
}

function children(pid: number): number[] {
  const out: number[] = [];
  const tasks = (() => {
    try {
      return readdirSync(`/proc/${pid}/task`);
    } catch {
      return [];
    }
  })();
  for (const t of tasks) {
    const c = read(`/proc/${pid}/task/${t}/children`);
    if (c) for (const p of c.trim().split(/\s+/)) if (p) out.push(Number(p));
  }
  return out;
}

/** The app's pid and every process under it. */
export function tree(root: number): number[] {
  const seen: number[] = [];
  const stack = [root];
  while (stack.length) {
    const p = stack.pop()!;
    if (seen.includes(p)) continue;
    seen.push(p);
    stack.push(...children(p));
  }
  return seen;
}

function kib(text: string, key: string): number {
  const m = text.match(new RegExp(`^${key}:\\s+(\\d+) kB`, "m"));
  return m ? Number(m[1]) : 0;
}

/** Unit-aware value of a `drm-*` fdinfo line, in KiB. */
function drmKiB(value: string): number {
  const m = value.trim().match(/^(\d+)\s*(KiB|MiB|GiB)?$/);
  if (!m) return 0;
  const n = Number(m[1]);
  return m[2] === "GiB"
    ? n * 1048576
    : m[2] === "MiB"
      ? n * 1024
      : m[2] === "KiB"
        ? n
        : n / 1024;
}

/** GPU memory resident for this process and its render-engine time.
 *  One client may appear on several fds (dup'd): `drm-client-id`
 *  dedupes them. */
function gpu(pid: number): { residentKiB: number; renderNs: number } {
  let fds: string[] = [];
  try {
    fds = readdirSync(`/proc/${pid}/fdinfo`);
  } catch {
    return { residentKiB: 0, renderNs: 0 };
  }
  const clients = new Map<string, { residentKiB: number; renderNs: number }>();
  for (const fd of fds) {
    const t = read(`/proc/${pid}/fdinfo/${fd}`);
    if (!t || !t.includes("drm-driver:")) continue;
    const id = t.match(/^drm-client-id:\s+(\d+)/m)?.[1] ?? `fd${fd}`;
    let residentKiB = 0;
    let renderNs = 0;
    for (const line of t.split("\n")) {
      const [k, v] = line.split(/:\s+/, 2);
      if (!v) continue;
      if (/^drm-resident-/.test(k)) residentKiB += drmKiB(v);
      if (/^drm-engine-(render|gfx)$/.test(k))
        renderNs += Number(v.split(" ")[0]) || 0;
    }
    clients.set(id, { residentKiB, renderNs });
  }
  let residentKiB = 0;
  let renderNs = 0;
  for (const c of clients.values()) {
    residentKiB += c.residentKiB;
    renderNs += c.renderNs;
  }
  return { residentKiB, renderNs };
}

export function sample(pid: number): ProcSample | null {
  const stat = read(`/proc/${pid}/stat`);
  if (!stat) return null;
  const close = stat.lastIndexOf(")");
  const name = stat.slice(stat.indexOf("(") + 1, close);
  const f = stat.slice(close + 2).split(" ");
  const cpuTicks = Number(f[11]) + Number(f[12]);
  const roll = read(`/proc/${pid}/smaps_rollup`) ?? "";
  const g = gpu(pid);
  return {
    pid,
    name,
    rssKiB: kib(roll, "Rss"),
    pssKiB: kib(roll, "Pss"),
    cpuTicks,
    gpuResidentKiB: g.residentKiB,
    gpuRenderNs: g.renderNs,
  };
}

export interface TreeSample {
  t: number;
  procs: ProcSample[];
}

export function sampleTree(root: number): TreeSample {
  return {
    t: Date.now(),
    procs: tree(root)
      .map(sample)
      .filter((s): s is ProcSample => s != null),
  };
}

/** Group a tree sample by role: the UI process, the web processes, the
 *  rest (network, GPU helpers). */
export function byRole(s: TreeSample, root: number) {
  const role = (p: ProcSample) =>
    p.pid === root
      ? "ui"
      : /WebKitWebProces|WebKitWebProcess/.test(p.name)
        ? "web"
        : "other";
  const out: Record<
    string,
    {
      pssKiB: number;
      rssKiB: number;
      cpuTicks: number;
      gpuResidentKiB: number;
      gpuRenderNs: number;
    }
  > = {};
  for (const p of s.procs) {
    const r = role(p);
    const o = (out[r] ??= {
      pssKiB: 0,
      rssKiB: 0,
      cpuTicks: 0,
      gpuResidentKiB: 0,
      gpuRenderNs: 0,
    });
    o.pssKiB += p.pssKiB;
    o.rssKiB += p.rssKiB;
    o.cpuTicks += p.cpuTicks;
    o.gpuResidentKiB += p.gpuResidentKiB;
    o.gpuRenderNs += p.gpuRenderNs;
  }
  return out;
}

export const TICKS_PER_SECOND = CLK_TCK;

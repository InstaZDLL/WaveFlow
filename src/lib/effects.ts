/**
 * How much the interface draws besides its content: blurs, glass,
 * glows, the atmosphere behind a skin, the animations that never stop.
 *
 * With a GPU these cost little. Without one — the software renderer the
 * fallback of #595 puts a machine on, a virtual machine, a driver that
 * renders on the CPU — each costs the whole frame it repaints in, and
 * the WebKitGTK bench (`scripts/bench/webkitgtk`) measured skins drawing
 * a dozen frames a second there. The reduced level keeps the layout,
 * the colours and the content, and draws each surface flat.
 *
 * ## Three states, like the contrast
 *
 * `auto` (the default) reduces when the machine draws without a GPU,
 * and only then; the two others are explicit and win in both
 * directions. The attribute stamped on the root carries the RESOLVED
 * state, `full` or `reduced`, so the stylesheet never has to know what
 * `auto` resolved to.
 *
 * ## What `auto` knows
 *
 * - The native side passes `renderer=software` in the main window's URL
 *   when this launch draws without the GPU (forced, or the fallback), so
 *   the bootstrap in `index.html` can stamp the first paint right.
 * - The main window hands the parameter on to the mini-player it opens,
 *   and every window also asks `renderer_status` at once, for the one
 *   that carries no hint.
 * - A WebGL context that names a software rasteriser (llvmpipe,
 *   SwiftShader, Microsoft's Basic Render Driver: a virtual machine, a
 *   missing driver) counts as no GPU too. That probe runs once, after
 *   the first paint, and its answer is cached for the next launch's
 *   bootstrap.
 */

/** What the user asked for, which is not the same as what is applied. */
export type EffectsLevel = "auto" | "full" | "reduced";

/** What is painted. */
export type ResolvedEffects = "full" | "reduced";

export const DEFAULT_EFFECTS_LEVEL: EffectsLevel = "auto";

/** `profile_setting` key holding the user's choice. */
export const EFFECTS_SETTING_KEY = "appearance.effects";

/** localStorage keys read by the bootstrap in `index.html`: the choice,
 *  and whether the last probe found a software rasteriser. */
export const EFFECTS_CACHE_KEY = "waveflow.effects";
export const EFFECTS_PROBE_CACHE_KEY = "waveflow.effects.software-gl";

export function parseEffectsLevel(raw: string | null): EffectsLevel {
  return raw === "auto" || raw === "full" || raw === "reduced"
    ? raw
    : DEFAULT_EFFECTS_LEVEL;
}

/** The main window's launch said it draws without the GPU. */
export function launchedInSoftware(): boolean {
  if (typeof window === "undefined") return false;
  return (
    new URLSearchParams(window.location.search).get("renderer") === "software"
  );
}

/** A WebGL renderer string naming a rasteriser that runs on the CPU. */
const SOFTWARE_GL =
  /llvmpipe|softpipe|lavapipe|swiftshader|basic render|software/i;

/**
 * Whether the WebGL context this engine hands out is drawn on the CPU.
 * `null` when nothing can be said: no WebGL at all, or an engine that
 * does not name its renderer.
 */
export function probeSoftwareGl(): boolean | null {
  if (typeof document === "undefined") return null;
  try {
    const canvas = document.createElement("canvas");
    const gl = canvas.getContext("webgl");
    if (!gl) return null;
    const info = gl.getExtension("WEBGL_debug_renderer_info");
    const name = info
      ? gl.getParameter(info.UNMASKED_RENDERER_WEBGL)
      : gl.getParameter(gl.RENDERER);
    // The context is released at once: it was only asked its name.
    gl.getExtension("WEBGL_lose_context")?.loseContext();
    return typeof name === "string" ? SOFTWARE_GL.test(name) : null;
  } catch {
    return null;
  }
}

export function readCachedProbe(): boolean {
  if (typeof window === "undefined") return false;
  try {
    return window.localStorage.getItem(EFFECTS_PROBE_CACHE_KEY) === "1";
  } catch {
    return false;
  }
}

export function writeCachedProbe(software: boolean) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(EFFECTS_PROBE_CACHE_KEY, software ? "1" : "0");
  } catch {
    // Only the next launch's first paint is affected.
  }
}

/** Turn the stored choice into the state painted, given whether this
 *  machine draws without a GPU. */
export function resolveEffects(
  level: EffectsLevel,
  noGpu: boolean,
): ResolvedEffects {
  if (level === "full" || level === "reduced") return level;
  return noGpu ? "reduced" : "full";
}

export function applyEffects(resolved: ResolvedEffects) {
  if (typeof document === "undefined") return;
  document.documentElement.setAttribute("data-effects", resolved);
}

export function writeCachedEffects(level: EffectsLevel) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(EFFECTS_CACHE_KEY, level);
  } catch {
    // The DB row still wins on the next launch.
  }
}

/** Whether the document is drawn with reduced effects right now. */
export function effectsReduced(): boolean {
  if (typeof document === "undefined") return false;
  return document.documentElement.getAttribute("data-effects") === "reduced";
}

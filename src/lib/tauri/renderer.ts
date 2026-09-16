import { invoke } from "@tauri-apps/api/core";

/** How the interface is being drawn. */
export type RenderMode = "gpu" | "software";

/** Why that mode was chosen. */
export type RenderReason =
  /** Nothing asked for anything else. */
  | "default"
  /** `WAVEFLOW_RENDERER` named it. */
  | "forced"
  /** The previous launch never reported a paint, so this one fell back. */
  | "previous-launch-never-painted"
  /** A software launch painted, so this one did not try the GPU. */
  | "remembered"
  /** Software did not paint either, so this one is back on the default. */
  | "software-did-not-help"
  /** The fallback was called for and this platform has none. */
  | "software-unavailable";

export interface RendererStatus {
  mode: RenderMode;
  reason: RenderReason;
  /**
   * Whether `rendererRetryGpu` has anything to undo. False when the mode
   * came from the environment variable — the stored state is not what is
   * deciding then, so the button would change nothing.
   */
  canRetryGpu: boolean;
}

/**
 * How this launch is drawing, and why.
 *
 * `null` when the decision never ran (no app-data directory), in which
 * case the UI shows nothing rather than guessing at a mode.
 */
export function rendererStatus(): Promise<RendererStatus | null> {
  return invoke<RendererStatus | null>("renderer_status");
}

/**
 * Forget the software fallback so the next launch tries the GPU again.
 *
 * Takes effect on the **next** start: the web engine read its
 * environment when its process began and nothing can move it now, which
 * is why every caller has to say so rather than imply the change is
 * live.
 */
export function rendererRetryGpu(): Promise<void> {
  return invoke<void>("renderer_retry_gpu");
}

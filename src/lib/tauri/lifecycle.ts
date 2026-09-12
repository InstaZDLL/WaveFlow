import { invoke } from "@tauri-apps/api/core";
import { readStartupTimings } from "../startupTiming";

/**
 * Tell the backend the frontend has rendered, so it reveals the main
 * window and closes the splash (#626).
 *
 * A command rather than the `app://ready` event it replaces: the command
 * handler is registered at build time, whereas the backend's event
 * listener is registered part-way through `setup` — after three blocking
 * database reads and the audio device open — while this webview is already
 * running. An event handled before that listener exists is dropped, and
 * the user then waits out the 15 s safety net.
 *
 * `sinceNavigationMs` is this side's own measurement of how long it took
 * to get here, and the two marks that come with it say where that time
 * went: the entry module executing, and i18next resolving. Measured
 * launches put the ready signal 20 to 27 s after navigation with `setup`
 * long finished, so the split is the only way to name the phase that is
 * slow rather than guess at it.
 */
export async function markFrontendReady(
  sinceNavigationMs: number,
): Promise<void> {
  const { bundleMs, i18nMs } = readStartupTimings();
  await invoke("app_ready", {
    sinceNavigationMs,
    bundleMs,
    i18nMs,
  });
}

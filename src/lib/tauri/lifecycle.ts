import { invoke } from "@tauri-apps/api/core";

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
 * to get here. The backend logs it next to its own elapsed time, which is
 * what tells "the frontend was slow" apart from "the signal was lost".
 */
export async function markFrontendReady(
  sinceNavigationMs: number,
): Promise<void> {
  await invoke("app_ready", { sinceNavigationMs });
}

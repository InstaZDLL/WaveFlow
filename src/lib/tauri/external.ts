import { invoke } from "@tauri-apps/api/core";

/**
 * Open an `http`, `https` or `mailto` link with the desktop's handler.
 *
 * Goes through the backend rather than the opener plugin's `openUrl`:
 * inside an AppImage the browser has to be started with the host's
 * libraries, which only the backend can arrange.
 */
export function openUrl(url: string): Promise<void> {
  return invoke<void>("open_external_url", { url });
}

import { listen } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

/**
 * Window event carrying a `profile_setting` key that another **webview**
 * just wrote. `detail.key` says which one.
 *
 * Every preference broadcasts on its own `window` event after a write, and
 * `window` is per document. The mini-player (`?mini=1`) and the desktop
 * lyrics overlay (`?lyrics=1`) are separate `WebviewWindow`s, so a setting
 * changed in the main window never reached them: they re-read it on mount
 * and on a profile switch, and otherwise kept showing the old one until
 * they were closed and reopened (issue #741).
 *
 * This is not one setting — it is every preference those windows read. The
 * cover slideshow is simply the one a tester noticed.
 */
export const PROFILE_SETTING_CHANGED = "waveflow:profile-setting-changed";

interface ProfileSettingChanged {
  key: string;
  /** Label of the window that wrote it. */
  source: string;
}

let started = false;

/**
 * Bridge the backend's `profile-setting:changed` into the `window` event
 * above, once per document.
 *
 * Idempotent, and called from every `useProfileSetting` mount rather than
 * from a module side effect: that way it exists wherever a preference is
 * actually read, including in the two windows this exists for, without
 * anyone having to remember to install it there.
 *
 * The window that made the change ignores its own echo. It painted the
 * value optimistically and already told its own consumers; re-reading
 * would be a wasted round-trip, and the read is what the writer's token
 * guards are there to keep out of the way of a newer toggle.
 */
export function startProfileSettingBridge(): void {
  if (started) return;
  started = true;

  const self = (() => {
    try {
      return getCurrentWebviewWindow().label;
    } catch {
      // Not in a Tauri webview (a plain `vite dev` page). Nothing emits
      // there either, so an empty label matches nothing and the bridge
      // is simply inert.
      return "";
    }
  })();

  void listen<ProfileSettingChanged>("profile-setting:changed", (event) => {
    if (event.payload.source === self) return;
    window.dispatchEvent(
      new CustomEvent(PROFILE_SETTING_CHANGED, {
        detail: { key: event.payload.key },
      }),
    );
  }).catch((err: unknown) => {
    // A window without the `event` capability would land here. Leaving
    // `started` true is deliberate: retrying on every mount would spam
    // the same failure once per preference hook.
    console.error("[profileSettingBridge] listen failed", err);
  });
}

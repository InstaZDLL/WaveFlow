/**
 * Window event announcing that a preference was just written through a
 * backend command of its own — the audio engine's setters, the app-wide
 * switches, the DLNA and MPD configs.
 *
 * A write through `set_profile_setting` already announces itself: the
 * backend emits `profile-setting:changed` for every one. These commands
 * write their row (or the OS autostart entry, or a file) themselves and
 * emit nothing, so the Settings header that counts the settings differing
 * from their default would not hear about them. Each of their wrappers
 * goes through {@link announceSettingWrite} instead.
 */
export const SETTING_WRITTEN_EVENT = "waveflow:setting-written";

/** Resolve `write`, then announce it. A failed write announces nothing:
 *  the value it would have changed is still the one already counted. */
export async function announceSettingWrite<T>(write: Promise<T>): Promise<T> {
  const result = await write;
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent(SETTING_WRITTEN_EVENT));
  }
  return result;
}

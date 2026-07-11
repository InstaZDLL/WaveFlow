import { invoke } from "@tauri-apps/api/core";

export function getMinimizeToTray(): Promise<boolean> {
  return invoke<boolean>("get_minimize_to_tray");
}

export function setMinimizeToTray(enabled: boolean): Promise<void> {
  return invoke<void>("set_minimize_to_tray", { enabled });
}

export function getAutoStart(): Promise<boolean> {
  return invoke<boolean>("get_auto_start");
}

export function setAutoStart(enabled: boolean): Promise<void> {
  return invoke<void>("set_auto_start", { enabled });
}

/** UI zoom level. Backend stores it in `app_setting` clamped to
 *  [0.5, 2.0]; the frontend mirrors the same bounds on the Settings
 *  slider and the keyboard-shortcut step, so all writes go through
 *  the same validated path. */
export const UI_ZOOM_MIN = 0.5;
export const UI_ZOOM_MAX = 2.0;
export const UI_ZOOM_STEP = 0.1;

export function getUiZoom(): Promise<number> {
  return invoke<number>("get_ui_zoom");
}

export function setUiZoom(zoom: number): Promise<void> {
  return invoke<void>("set_ui_zoom", { zoom });
}

/** Window-level event the keyboard shortcut handler dispatches every
 *  time it nudges the zoom, so the Settings card stays in sync
 *  without us having to plumb a context through. */
export const UI_ZOOM_CHANGED_EVENT = "waveflow:ui-zoom-changed";

/** Mini-player window bounds in logical pixels. Persisted as a single
 *  JSON blob so the four fields move as one row — restoring half a
 *  position is worse than restoring none of it. */
export interface MiniPlayerBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

export function getMiniPlayerBounds(): Promise<MiniPlayerBounds | null> {
  return invoke<MiniPlayerBounds | null>("get_mini_player_bounds");
}

export function setMiniPlayerBounds(bounds: MiniPlayerBounds): Promise<void> {
  return invoke<void>("set_mini_player_bounds", { bounds });
}

export function getMainWindowBounds(): Promise<MiniPlayerBounds | null> {
  return invoke<MiniPlayerBounds | null>("get_main_window_bounds");
}

export function setMainWindowBounds(bounds: MiniPlayerBounds): Promise<void> {
  return invoke<void>("set_main_window_bounds", { bounds });
}

import { invoke } from "@tauri-apps/api/core";

/**
 * The floating desktop lyrics window (issue #582). The backend owns it —
 * the tray menu drives it too — so every surface goes through these
 * calls and follows {@link DESKTOP_LYRICS_STATE_EVENT} rather than
 * tracking the window itself.
 */
export interface DesktopLyricsStatus {
  open: boolean;
  /** Click-through: the window ignores the mouse until unlocked from
   *  the tray, the player bar's "⋯" menu or Settings. */
  locked: boolean;
  /** A native Wayland session: the compositor may ignore "always on top"
   *  and choose the window's position. */
  wayland: boolean;
}

/** Emitted by the backend to every window on open, close and lock. */
export const DESKTOP_LYRICS_STATE_EVENT = "desktop-lyrics:state";

export function getDesktopLyricsStatus(): Promise<DesktopLyricsStatus> {
  return invoke<DesktopLyricsStatus>("desktop_lyrics_status");
}

export function openDesktopLyrics(): Promise<void> {
  return invoke<void>("open_desktop_lyrics");
}

export function closeDesktopLyrics(): Promise<void> {
  return invoke<void>("close_desktop_lyrics");
}

export function setDesktopLyricsLocked(locked: boolean): Promise<void> {
  return invoke<void>("set_desktop_lyrics_locked", { locked });
}

export interface DesktopLyricsBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

export function setDesktopLyricsBounds(
  bounds: DesktopLyricsBounds,
): Promise<void> {
  return invoke<void>("set_desktop_lyrics_bounds", { bounds });
}

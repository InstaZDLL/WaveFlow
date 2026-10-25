import { invoke } from "@tauri-apps/api/core";

/** What a sidebar pin points at. Mirrors `PinKind` in core. */
export type PinKind = "album" | "artist" | "playlist";

/**
 * One pinned entry, in pinning order. Mirrors
 * `commands::sidebar_pins::SidebarPin`.
 *
 * An album or an artist with no available track is left out, rather than
 * shown as a row that opens on nothing; its pin stays, and the row comes
 * back with the tracks. A playlist pin carries its id alone: the sidebar
 * draws it from the playlist list it already holds.
 */
export interface SidebarPin {
  kind: PinKind;
  id: number;
  name: string;
  /** The album's artist; `null` for an artist or a playlist. */
  subtitle: string | null;
  /** Album cover or artist picture, as a full path, best first. */
  image_path: string | null;
  image_path_1x: string | null;
  image_path_2x: string | null;
}

export function listSidebarPins(): Promise<SidebarPin[]> {
  return invoke<SidebarPin[]>("list_sidebar_pins");
}

/** Pin or unpin. Pinning twice keeps the first place in the list. */
export function setSidebarPin(
  kind: PinKind,
  id: number,
  pinned: boolean,
): Promise<void> {
  return invoke<void>("set_sidebar_pin", { kind, id, pinned });
}

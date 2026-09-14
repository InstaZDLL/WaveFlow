import { invoke } from "@tauri-apps/api/core";
import {
  expandLibraryTrackRows,
  type ListLibraryTracksResponse,
  type LibraryTrackRow,
} from "./browse";

/**
 * One line of the "needs attention" inventory (issue #589).
 *
 * `key` is a stable slug the UI turns into localized copy — treat it
 * like an event name, not a label.
 */
export interface InventoryCategory {
  key: string;
  count: number;
}

/**
 * Counts per category, over the whole library.
 *
 * Genuinely expensive: several of these are album-level aggregates and
 * one walks every track to chain probable duplicates. Called when the
 * inventory is opened, not on every mount of the library — unlike the
 * other library tabs, which prefetch in parallel.
 */
export function inventorySummary(): Promise<InventoryCategory[]> {
  return invoke<InventoryCategory[]>("inventory_summary");
}

/**
 * The tracks of one category, in the library table's own shape.
 *
 * Goes through the same row expander as the Tracks tab and the folder
 * browser, so the inventory renders in the library's own table with its
 * columns, its sort and its context menu — the point being that it is
 * an entry point and not a report.
 */
export async function inventoryTracks(
  category: string,
  sort?: { orderBy: string; direction: "asc" | "desc" },
): Promise<LibraryTrackRow[]> {
  const resp = await invoke<ListLibraryTracksResponse>("inventory_tracks", {
    category,
    orderBy: sort?.orderBy ?? null,
    direction: sort?.direction ?? null,
  });
  return expandLibraryTrackRows(resp);
}

import { invoke } from "@tauri-apps/api/core";

/**
 * Library row returned by the Rust backend (mirrors `commands::library::Library`).
 *
 * `track_count`, `album_count` and `folder_count` are computed on the fly by
 * the SQL query — they reflect the state at fetch time, not a cached column.
 */
export interface Library {
  id: number;
  name: string;
  description: string | null;
  color_id: string;
  icon_id: string;
  created_at: number;
  updated_at: number;
  track_count: number;
  album_count: number;
  artist_count: number;
  genre_count: number;
  folder_count: number;
}

export interface CreateLibraryInput {
  name: string;
  description?: string | null;
  color_id?: string;
  icon_id?: string;
}

/** Partial update payload — any omitted field is preserved as-is. */
export interface UpdateLibraryInput {
  name?: string;
  description?: string | null;
  color_id?: string;
  icon_id?: string;
}

/** Outcome of a folder scan, returned by `scan_folder`. */
export interface ScanSummary {
  folder_id: number;
  scanned: number;
  added: number;
  updated: number;
  skipped: number;
  errors: number;
}

/** Aggregate result of `rescan_library` — summed across every folder. */
export interface RescanSummary {
  library_id: number;
  folders: number;
  scanned: number;
  added: number;
  updated: number;
  skipped: number;
  errors: number;
}

export function listLibraries(): Promise<Library[]> {
  return invoke<Library[]>("list_libraries");
}

export function createLibrary(input: CreateLibraryInput): Promise<Library> {
  return invoke<Library>("create_library", { input });
}

export function updateLibrary(
  libraryId: number,
  input: UpdateLibraryInput
): Promise<void> {
  return invoke<void>("update_library", { libraryId, input });
}

export function deleteLibrary(libraryId: number): Promise<void> {
  return invoke<void>("delete_library", { libraryId });
}

export function rescanLibrary(libraryId: number): Promise<RescanSummary> {
  return invoke<RescanSummary>("rescan_library", { libraryId });
}

export function addFolderToLibrary(
  libraryId: number,
  path: string
): Promise<number> {
  return invoke<number>("add_folder_to_library", { libraryId, path });
}

export function scanFolder(folderId: number): Promise<ScanSummary> {
  return invoke<ScanSummary>("scan_folder", { folderId });
}

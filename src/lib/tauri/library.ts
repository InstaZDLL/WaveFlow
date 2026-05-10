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
  /** Tracks the scanner could no longer find on disk and flagged
   *  `is_available = 0`. Their rows stay around so the user keeps
   *  liked / playlist / play-event history if the file comes back. */
  removed: number;
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
  removed: number;
}

export function listLibraries(): Promise<Library[]> {
  return invoke<Library[]>("list_libraries");
}

export function createLibrary(input: CreateLibraryInput): Promise<Library> {
  return invoke<Library>("create_library", { input });
}

export function updateLibrary(
  libraryId: number,
  input: UpdateLibraryInput,
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
  path: string,
): Promise<number> {
  return invoke<number>("add_folder_to_library", { libraryId, path });
}

export function scanFolder(folderId: number): Promise<ScanSummary> {
  return invoke<ScanSummary>("scan_folder", { folderId });
}

/**
 * Per-library folder row used by the folder management UI: just the
 * raw `library_folder` columns the user can see and act on (path,
 * last scan timestamp, watch flag). Counts come from `listFolders`.
 */
export interface LibraryFolder {
  id: number;
  library_id: number;
  path: string;
  last_scanned_at: number | null;
  is_watched: number;
}

export function listLibraryFolders(
  libraryId: number,
): Promise<LibraryFolder[]> {
  return invoke<LibraryFolder[]>("list_library_folders", { libraryId });
}

/**
 * Toggle whether a folder is watched for filesystem changes. The
 * backend updates `library_folder.is_watched` and (un)mounts the
 * notify watcher in one call so the change takes effect immediately.
 */
export function setFolderWatched(
  folderId: number,
  enable: boolean,
): Promise<void> {
  return invoke<void>("set_folder_watched", { folderId, enable });
}

/**
 * Detach the watcher, delete every track that lived under this
 * folder, then drop the `library_folder` row. Emits
 * `library:rescanned` so consumer views auto-refresh.
 */
export function removeFolderFromLibrary(folderId: number): Promise<void> {
  return invoke<void>("remove_folder_from_library", { folderId });
}

/**
 * Import a list of arbitrary filesystem paths into a library. Used
 * by the drag-and-drop handler — accepts a mix of folders and audio
 * files (files contribute their parent directory). Adds each as a
 * library_folder (skipping duplicates) and triggers a scan, then
 * returns a single aggregated summary.
 */
export function importPaths(
  libraryId: number,
  paths: string[],
): Promise<{
  scanned: number;
  added: number;
  updated: number;
  skipped: number;
  errors: number;
  removed: number;
}> {
  return invoke("import_paths", { libraryId, paths });
}

/**
 * Walk every artwork directory and (re)build the `_1x` / `_2x` JPEG
 * thumbnails for any full-size cover that is missing them. Returns
 * the count of source images successfully processed.
 */
export function regenerateThumbnails(): Promise<number> {
  return invoke<number>("regenerate_thumbnails");
}

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
  /** The walk stopped early, so the counters cover only the folders it reached. */
  cancelled: boolean;
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

/**
 * Rescan every folder of a library.
 *
 * `deep` bypasses the `(mtime, size)` fast path so every file is
 * re-hashed and re-read. Needed when an external tagger rewrote tags
 * while preserving mtime — Mp3tag does that by default — which the
 * normal pass cannot see (issue #457). Slower, so it stays opt-in.
 */
export function rescanLibrary(
  libraryId: number,
  deep = false,
): Promise<RescanSummary> {
  return invoke<RescanSummary>("rescan_library", { libraryId, deep });
}

/**
 * Register a folder of a library. Refused with a `FolderOverlap` error
 * (see {@link parseFolderOverlap}) when it is, holds or lies under a
 * folder already in the library.
 */
export function addFolderToLibrary(
  libraryId: number,
  path: string,
): Promise<number> {
  return invoke<number>("add_folder_to_library", { libraryId, path });
}

/** How a folder lies relative to the library folder in its way. */
export type FolderRelation = "same" | "inside" | "contains";

export interface FolderOverlap {
  relation: FolderRelation;
  /** The library folder in the way. */
  folder: string;
}

/**
 * The library folder an add or a drop ran into, read from the backend's
 * `AppError::FolderOverlap` message, whose wording is the contract; `null`
 * for any other error.
 */
export function parseFolderOverlap(err: unknown): FolderOverlap | null {
  const match =
    /^the folder (is inside|is|contains) the library folder (.+)$/.exec(
      String(err),
    );
  if (!match) return null;
  const relation: FolderRelation =
    match[1] === "is"
      ? "same"
      : match[1] === "is inside"
        ? "inside"
        : "contains";
  return { relation, folder: match[2] };
}

/**
 * Something the import surfaces should tell the user: a folder refused
 * for overlapping one in the library, or what a drop copied. Dispatched on
 * `window` as {@link IMPORT_NOTICE_EVENT}, so every place that adds a
 * folder is covered by one toast without knowing about it.
 */
export type ImportNotice =
  | { kind: "overlap"; overlap: FolderOverlap }
  | { kind: "dropped"; summary: DropImportSummary };

export const IMPORT_NOTICE_EVENT = "waveflow:import-notice";

export function announceImport(notice: ImportNotice): void {
  window.dispatchEvent(
    new CustomEvent(IMPORT_NOTICE_EVENT, { detail: notice }),
  );
}

/**
 * `deep: true` bypasses the scanner's `(mtime, size)` fast path so every
 * file in the folder is fully re-hashed and re-read (issue #366) — the
 * only way to catch a tag or embedded-picture edit made by an external
 * tool that preserved the file's modification time. Slower than a
 * normal rescan; meant as an occasional, explicit user action.
 */
export function scanFolder(
  folderId: number,
  deep?: boolean,
): Promise<ScanSummary> {
  return invoke<ScanSummary>("scan_folder", { folderId, deep });
}

/** Outcome of {@link rescanLocalArtistImages}. */
export interface ArtistImageScanSummary {
  /** Artists whose `artwork_id` was NULL when the rescan started. */
  considered: number;
  /** Artists that now have a sidecar image linked. */
  linked: number;
}

/**
 * Walk every artist row that still has no local artwork and try to
 * resolve a sidecar `artist.jpg` (or `<artist_name>.jpg`) from any
 * folder along their tracks' paths. Lets libraries scanned before the
 * local-artist-image feature shipped pick up the sidecars without
 * re-importing every folder.
 */
export function rescanLocalArtistImages(): Promise<ArtistImageScanSummary> {
  return invoke<ArtistImageScanSummary>("rescan_local_artist_images");
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
 * Number of listening events attached to the tracks in a folder.
 *
 * Shown in the folder delete confirmation. The history survives the
 * delete and is re-attached on the next scan (issue #367), so this is
 * information, not a warning that data is about to vanish.
 */
export function countFolderPlayEvents(folderId: number): Promise<number> {
  return invoke<number>("count_folder_play_events", { folderId });
}

/** What a drop added to the library: `import_dropped`'s summary. */
export interface DropImportSummary {
  /** The folder WaveFlow manages, where the files were copied. */
  folder: string;
  copied: number;
  /** Already in the managed folder from an earlier drop. */
  already_copied: number;
  /** Dropped items already under a library folder, passed over. */
  in_library: number;
  /** Dropped folders holding a library folder, refused. */
  refused: number;
  errors: number;
  /** The drop held more files than one drop copies. */
  truncated: boolean;
  cancelled: boolean;
  scan: ScanSummary | null;
}

/**
 * Add dropped files and folders to a library by copying them into the
 * folder WaveFlow manages under the user's music (`Music/WaveFlow`), then
 * scanning it. The folders they came from are never registered.
 */
export function importDropped(
  libraryId: number,
  paths: string[],
): Promise<DropImportSummary> {
  return invoke<DropImportSummary>("import_dropped", { libraryId, paths });
}

/**
 * Walk every artwork directory and (re)build the `_1x` / `_2x` JPEG
 * thumbnails for any full-size cover that is missing them. Returns
 * the count of source images successfully processed.
 */
export function regenerateThumbnails(): Promise<number> {
  return invoke<number>("regenerate_thumbnails");
}

export interface AlbumCoverCleanup {
  rowsCleared: number;
  filesDeleted: number;
  bytesFreed: number;
}

/**
 * One-time cleanup (issue #493): delete the Deezer album covers the shared
 * `metadata_artwork` cache accumulated for albums that already have their own
 * local artwork — files that were downloaded as a side effect and never shown.
 * Reference-counted, so a file shared with an artist picture or reused across
 * albums is never removed.
 */
export function pruneCachedAlbumCovers(): Promise<AlbumCoverCleanup> {
  return invoke<AlbumCoverCleanup>("prune_cached_album_covers");
}

/**
 * Wipe every profile, library, playlist and cache, then restart the
 * app into onboarding. The backend never returns — `app.restart()`
 * swaps the process — so the returned Promise resolves only if the
 * teardown failed before the restart point (e.g. couldn't delete the
 * data dir because a file was held by another process).
 *
 * The caller is responsible for showing a confirmation dialog before
 * invoking this; the backend trusts that the user already confirmed.
 */
export function resetApp(): Promise<void> {
  return invoke<void>("reset_app");
}

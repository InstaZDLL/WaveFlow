import { invoke } from "@tauri-apps/api/core";

/**
 * Bundle the active profile (or `profileId` when given) into a
 * `.waveflow` archive at `targetPath`. Throws on I/O failure.
 */
export function exportProfile(
  targetPath: string,
  profileId?: number | null,
): Promise<void> {
  return invoke<void>("export_profile", {
    profileId: profileId ?? null,
    targetPath,
  });
}

/** The profile an import created. `name` is the archive's own unless
 * another profile already held it, in which case it gains a " (2)"-style
 * suffix (#767). */
export interface ImportedProfile {
  id: number;
  name: string;
}

/**
 * Import a `.waveflow` archive as a brand-new profile. Returns the
 * new profile's id and name. The new profile is **not** auto-activated
 * — the caller decides when to switch.
 */
export function importProfile(
  sourcePath: string,
  name?: string | null,
): Promise<ImportedProfile> {
  return invoke<ImportedProfile>("import_profile", {
    sourcePath,
    name: name ?? null,
  });
}

/** Mirrors `profile_io::ArchiveCompatibility`: whether `importProfile`
 * would take the archive, and if not, why. */
export type ArchiveCompatibility =
  | { kind: "compatible" }
  | { kind: "newerArchive" }
  | { kind: "unsupportedArchive" }
  | { kind: "newerSchema"; version: number }
  | { kind: "differentBuild"; version: number };

/** Row counts of the archive's database. */
export interface ArchiveContents {
  tracks: number;
  playlists: number;
  smartPlaylists: number;
  likedTracks: number;
  plays: number;
  libraryFolders: number;
  settings: number;
}

/** Mirrors `profile_io::ArchivePreview`. */
export interface ArchivePreview {
  profileName: string;
  appVersion: string;
  /** RFC 3339. */
  exportedAt: string;
  archiveVersion: number;
  schemaVersion: number | null;
  localSchemaVersion: number;
  compatibility: ArchiveCompatibility;
  /** `null` when the archive's layout is one this build does not read. */
  contents: ArchiveContents | null;
  /** Entry counts; `null`, like `contents`, for a layout this build does
   * not read. */
  artworkFiles: number | null;
  metadataArtworkFiles: number | null;
  sizeBytes: number;
}

/** Prefix of the error a preview throws for a file that is not a readable
 * archive (not a zip, no manifest or database, a damaged database). */
export const ARCHIVE_UNREADABLE = "archive: unreadable";

/**
 * Read what a `.waveflow` archive holds without importing it. Read-only:
 * nothing is created or changed. Throws `ARCHIVE_UNREADABLE…` for a file
 * that is not a readable archive.
 */
export function previewProfileArchive(
  sourcePath: string,
): Promise<ArchivePreview> {
  return invoke<ArchivePreview>("preview_profile_archive", { sourcePath });
}

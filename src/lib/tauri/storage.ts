import { invoke } from "@tauri-apps/api/core";

/**
 * Where the rebuildable caches live (issue #619).
 *
 * Mirrors `commands::storage::CacheLocation`. Field names stay
 * snake_case because serde serializes them as-is.
 */
export interface CacheLocation {
  /** Being read and written right now. */
  active_root: string;
  /** Where they sit when nothing has been chosen. */
  default_root: string;
  /** The stored choice, when there is one. */
  configured_root: string | null;
  /** A stored choice existed but could not be used this session. */
  fell_back: boolean;
  /** Why, when it fell back — a drive that is unplugged and one that
   *  refuses writes call for different actions. */
  fallback_reason: string | null;
  /** A move has been staged and needs a restart to take effect. */
  restart_required: boolean;
  /** Bytes currently held by the caches, all families together. */
  size_bytes: number;
}

export function getCacheLocation(): Promise<CacheLocation> {
  return invoke<CacheLocation>("get_cache_location");
}

/**
 * Move the caches to `root`, or back to the default when `null`.
 *
 * Copies before it persists, and nothing is deleted until the next
 * launch — so an interrupted move leaves a whole copy on disk. The
 * returned `restart_required` is always `true` on a real move: the
 * backend resolves its paths once at boot, so the running process keeps
 * reading the old location until it is replaced.
 */
export function setCacheLocation(root: string | null): Promise<CacheLocation> {
  return invoke<CacheLocation>("set_cache_location", { root });
}

/** Replace the process so a staged move takes effect. Never resolves. */
export function restartForCacheMove(): Promise<void> {
  return invoke<void>("restart_for_cache_move");
}

/** One item of what WaveFlow keeps on disk. Mirrors the Rust
 *  `UsageEntry`: per-profile items are the active profile's. */
export interface DiskUsageEntry {
  id:
    | "artwork"
    | "metadataArtwork"
    | "motionCache"
    | "canvasCache"
    | "remoteArtwork"
    | "remoteStream"
    | "remoteDownloads"
    | "manualMedia"
    | "databases"
    | "databaseSnapshots"
    | "backups"
    | "logs"
    | "plugins";
  bytes: number;
  files: number;
  /** Emptied without loss: filled again from where it came from. */
  rebuildable: boolean;
  /** A folder could not be read whole: `bytes` is a floor. */
  incomplete: boolean;
}

export interface DiskUsage {
  entries: DiskUsageEntry[];
  totalBytes: number;
}

/** Measure every item. Walks the caches, so a few seconds at worst. */
export function diskUsage(): Promise<DiskUsage> {
  return invoke<DiskUsage>("disk_usage");
}

/** Window event fired after a cache is emptied from Settings, so every
 *  card showing a size on the page measures again. */
export const STORAGE_CHANGED_EVENT = "waveflow:storage-changed";

export function announceStorageChanged(): void {
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent(STORAGE_CHANGED_EVENT));
  }
}

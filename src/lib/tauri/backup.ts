import { invoke } from "@tauri-apps/api/core";

/** Mirrors `backup::BackupConfig` returned by `get_backup_config`. */
export interface BackupConfig {
  enabled: boolean;
  interval_days: number;
  folder: string;
  retention: number;
  /** Bundle the shared Deezer artwork cache into each archive. */
  include_metadata_artwork: boolean;
  /** Epoch ms of the last successful run; `0` if never. */
  last_run_at: number;
  /** Server-resolved default folder to suggest in the picker. */
  default_folder: string;
}

export function getBackupConfig(): Promise<BackupConfig> {
  return invoke<BackupConfig>("get_backup_config");
}

export function setBackupConfig(input: {
  enabled: boolean;
  interval_days: number;
  folder: string;
  retention: number;
  include_metadata_artwork: boolean;
}): Promise<void> {
  return invoke<void>("set_backup_config", { input });
}

/** Trigger a backup pass immediately. Returns the list of archive paths. */
/** Outcome of one manual backup pass. `created` can be empty for two
 *  different reasons, which is why `cancelled` comes with it: the user
 *  stopped the pass, or every profile failed. */
export interface BackupPass {
  created: string[];
  cancelled: boolean;
}

export function runBackupNow(): Promise<BackupPass> {
  return invoke<BackupPass>("run_backup_now");
}

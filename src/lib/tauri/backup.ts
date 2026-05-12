import { invoke } from "@tauri-apps/api/core";

/** Mirrors `backup::BackupConfig` returned by `get_backup_config`. */
export interface BackupConfig {
  enabled: boolean;
  interval_days: number;
  folder: string;
  retention: number;
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
}): Promise<void> {
  return invoke<void>("set_backup_config", { input });
}

/** Trigger a backup pass immediately. Returns the list of archive paths. */
export function runBackupNow(): Promise<string[]> {
  return invoke<string[]>("run_backup_now");
}

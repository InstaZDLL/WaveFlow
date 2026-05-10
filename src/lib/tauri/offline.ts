import { invoke } from "@tauri-apps/api/core";

export function getOfflineMode(): Promise<boolean> {
  return invoke<boolean>("get_offline_mode");
}

export function setOfflineMode(enabled: boolean): Promise<void> {
  return invoke<void>("set_offline_mode", { enabled });
}

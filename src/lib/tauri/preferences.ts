import { invoke } from "@tauri-apps/api/core";

export function getMinimizeToTray(): Promise<boolean> {
  return invoke<boolean>("get_minimize_to_tray");
}

export function setMinimizeToTray(enabled: boolean): Promise<void> {
  return invoke<void>("set_minimize_to_tray", { enabled });
}

export function getAutoStart(): Promise<boolean> {
  return invoke<boolean>("get_auto_start");
}

export function setAutoStart(enabled: boolean): Promise<void> {
  return invoke<void>("set_auto_start", { enabled });
}

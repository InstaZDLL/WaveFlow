import { invoke } from "@tauri-apps/api/core";

/** Arm Linux's abnormal-exit marker before applying a skin. */
export function recordSkin(skinId: string): Promise<void> {
  return invoke<void>("record_skin", { skinId });
}

/** Clear the marker only after Studio is saved to the active profile. */
export function completeSkinRecovery(): Promise<void> {
  return invoke<void>("complete_recovery");
}

import { invoke } from "@tauri-apps/api/core";

import { announceSettingWrite } from "../settingWrites";

export function getVisualizerEnabled(): Promise<boolean> {
  return invoke<boolean>("player_get_visualizer");
}

export function setVisualizerEnabled(
  enabled: boolean,
  expectedProfileId: number | null = null,
): Promise<void> {
  return announceSettingWrite(
    invoke<void>("player_set_visualizer", { enabled, expectedProfileId }),
  );
}

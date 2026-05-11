import { invoke } from "@tauri-apps/api/core";

export function getVisualizerEnabled(): Promise<boolean> {
  return invoke<boolean>("player_get_visualizer");
}

export function setVisualizerEnabled(enabled: boolean): Promise<void> {
  return invoke<void>("player_set_visualizer", { enabled });
}

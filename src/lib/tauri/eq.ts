import { invoke } from "@tauri-apps/api/core";

export interface EqPresetEntry {
  key: string;
  gains: number[];
}

export interface EqSnapshot {
  enabled: boolean;
  bands_db: number[];
  band_freqs: number[];
  max_gain_db: number;
  presets: EqPresetEntry[];
}

export function playerGetEq(): Promise<EqSnapshot> {
  return invoke<EqSnapshot>("player_get_eq");
}

export function playerSetEqEnabled(enabled: boolean): Promise<void> {
  return invoke<void>("player_set_eq_enabled", { enabled });
}

export function playerSetEqBand(index: number, gainDb: number): Promise<void> {
  return invoke<void>("player_set_eq_band", { index, gainDb });
}

export function playerSetEqPreset(presetKey: string): Promise<void> {
  return invoke<void>("player_set_eq_preset", { presetKey });
}

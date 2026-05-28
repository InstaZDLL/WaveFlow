import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

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

/**
 * Subscribe to backend-broadcast EQ snapshots. Fired after every
 * `playerSetEqEnabled` / `playerSetEqBand` / `playerSetEqPreset` so a
 * second surface (e.g. the player-bar popup) that mutated the engine
 * doesn't leave the Settings card showing stale state — and vice
 * versa. See issue #166.
 */
export function playerOnEq(
  handler: (snapshot: EqSnapshot) => void,
): Promise<UnlistenFn> {
  return listen<EqSnapshot>("player:eq", (event) => handler(event.payload));
}

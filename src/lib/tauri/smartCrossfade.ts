import { invoke } from "@tauri-apps/api/core";

import { announceSettingWrite } from "../settingWrites";

export function getSmartCrossfade(): Promise<boolean> {
  return invoke<boolean>("player_get_smart_crossfade");
}

export function setSmartCrossfade(enabled: boolean): Promise<void> {
  return announceSettingWrite(
    invoke<void>("player_set_smart_crossfade", { enabled }),
  );
}

export function getDynamicCrossfade(): Promise<boolean> {
  return invoke<boolean>("player_get_dynamic_crossfade");
}

export function setDynamicCrossfade(enabled: boolean): Promise<void> {
  return announceSettingWrite(
    invoke<void>("player_set_dynamic_crossfade", { enabled }),
  );
}

import { invoke } from "@tauri-apps/api/core";

import { announceSettingWrite } from "../settingWrites";

export function getSmartCrossfade(): Promise<boolean> {
  return invoke<boolean>("player_get_smart_crossfade");
}

export function setSmartCrossfade(
  enabled: boolean,
  expectedProfileId: number | null = null,
): Promise<void> {
  return announceSettingWrite(
    invoke<void>("player_set_smart_crossfade", { enabled, expectedProfileId }),
  );
}

export function getDynamicCrossfade(): Promise<boolean> {
  return invoke<boolean>("player_get_dynamic_crossfade");
}

export function setDynamicCrossfade(
  enabled: boolean,
  expectedProfileId: number | null = null,
): Promise<void> {
  return announceSettingWrite(
    invoke<void>("player_set_dynamic_crossfade", {
      enabled,
      expectedProfileId,
    }),
  );
}

export function getAutoMix(): Promise<boolean> {
  return invoke<boolean>("player_get_automix");
}

export function setAutoMix(
  enabled: boolean,
  expectedProfileId: number | null = null,
): Promise<void> {
  return announceSettingWrite(
    invoke<void>("player_set_automix", { enabled, expectedProfileId }),
  );
}

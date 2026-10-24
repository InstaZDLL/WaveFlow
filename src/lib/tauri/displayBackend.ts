import { invoke } from "@tauri-apps/api/core";

import { announceSettingWrite } from "../settingWrites";

/** The XWayland choice on a Linux Wayland session (#894). */
export interface DisplayBackendStatus {
  /** Linux on a Wayland session that runs XWayland: the only place the
   *  choice is offered. */
  available: boolean;
  /** The stored choice, applied from the next start. */
  xwayland: boolean;
  /** This launch runs through XWayland, whoever asked for it. */
  active: boolean;
  /** `GDK_BACKEND` came from the environment, which the stored choice
   *  never overrides. */
  set_by_user: boolean;
}

export function displayBackendStatus(): Promise<DisplayBackendStatus> {
  return invoke<DisplayBackendStatus>("display_backend_status");
}

/** Store the choice for the next start. */
export function displayBackendSetXwayland(
  enabled: boolean,
): Promise<DisplayBackendStatus> {
  return announceSettingWrite(
    invoke<DisplayBackendStatus>("display_backend_set_xwayland", {
      enabled,
    }),
  );
}

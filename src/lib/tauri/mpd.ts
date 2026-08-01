import { invoke } from "@tauri-apps/api/core";

/// Mirrors `mpd::config::MpdConfig`.
export interface MpdConfig {
  enabled: boolean;
  /// TCP port. Defaults to 6600, the MPD standard every client probes
  /// first. The server scans forward if it is taken, so the port that
  /// actually got bound is the one in `MpdStatus`.
  port: number;
  /// Empty string means no authentication.
  password: string;
}

/// Mirrors `mpd::MpdStatus`.
export interface MpdStatus {
  enabled: boolean;
  running: boolean;
  /// `host:port` reachable from the LAN — what the user types into
  /// their client. `null` while stopped.
  bound_address: string | null;
  port: number | null;
  last_error: string | null;
}

export function mpdGetConfig(): Promise<MpdConfig> {
  return invoke<MpdConfig>("mpd_get_config");
}

export function mpdSetConfig(cfg: MpdConfig): Promise<MpdStatus> {
  return invoke<MpdStatus>("mpd_set_config", { cfg });
}

export function mpdGetStatus(): Promise<MpdStatus> {
  return invoke<MpdStatus>("mpd_get_status");
}

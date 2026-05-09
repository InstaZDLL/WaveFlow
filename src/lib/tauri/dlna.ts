import { invoke } from "@tauri-apps/api/core";

/// Mirrors `dlna::config::DlnaConfig`.
export interface DlnaConfig {
  enabled: boolean;
  server_name: string;
  /// `0` lets the OS pick a free port; the SSDP LOCATION header
  /// carries whichever port actually ended up bound.
  port: number;
}

/// Mirrors `dlna::DlnaStatus`.
export interface DlnaStatus {
  enabled: boolean;
  running: boolean;
  server_name: string;
  bound_url: string | null;
  last_error: string | null;
}

export function dlnaGetConfig(): Promise<DlnaConfig> {
  return invoke<DlnaConfig>("dlna_get_config");
}

export function dlnaSetConfig(cfg: DlnaConfig): Promise<DlnaStatus> {
  return invoke<DlnaStatus>("dlna_set_config", { cfg });
}

export function dlnaGetStatus(): Promise<DlnaStatus> {
  return invoke<DlnaStatus>("dlna_get_status");
}

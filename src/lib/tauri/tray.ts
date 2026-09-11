import { invoke } from "@tauri-apps/api/core";

export interface TrayLabels {
  playPause: string;
  previous: string;
  next: string;
  show: string;
  quit: string;
  /** Tooltips of the Windows taskbar thumbnail's play/pause button. */
  play: string;
  pause: string;
}

export function setTrayLabels(labels: TrayLabels): Promise<void> {
  return invoke<void>("set_tray_labels", { labels });
}

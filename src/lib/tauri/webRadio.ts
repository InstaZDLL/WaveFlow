import { invoke } from "@tauri-apps/api/core";

export interface WebRadioStation {
  id: number;
  slug: string;
  name: string;
  tagline: string;
  genre: string;
  codec: string;
}

export function webRadioListStations(): Promise<WebRadioStation[]> {
  return invoke<WebRadioStation[]>("web_radio_list_stations");
}

export function webRadioPlayStation(stationId: number): Promise<void> {
  return invoke<void>("web_radio_play_station", { stationId });
}

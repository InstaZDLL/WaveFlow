import { invoke } from "@tauri-apps/api/core";

/**
 * Adding a Web Radio station by its URL or a playlist file
 * (`commands::web_radio_custom`). A station added this way is saved as a
 * favorite with the `url:<stream>` id the plugin already plays, so it is
 * played, renamed and removed like any other.
 */

/** A URL that answered as a station. */
export interface ProbedStation {
  /** What plays: the URL itself, or the stream a playlist named. */
  stream_url: string;
  /** The name the server announces (`icy-name`) or the playlist gives. */
  name: string | null;
}

/** One stream a `.m3u` / `.pls` file names. */
export interface RadioPlaylistEntry {
  url: string;
  title: string | null;
}

/** Open the URL once (headers only) and answer with the station behind it. */
export function probeRadioStation(url: string): Promise<ProbedStation> {
  return invoke<ProbedStation>("web_radio_probe_url", { url });
}

/** The absolute `http(s)` streams a playlist file on disk names. */
export function readRadioPlaylistFile(
  path: string,
): Promise<RadioPlaylistEntry[]> {
  return invoke<RadioPlaylistEntry[]>("web_radio_read_playlist_file", {
    path,
  });
}

/**
 * The i18n key of a refusal from the two commands above, by the prefix
 * the backend gives each (`ERR_*` in `web_radio_custom.rs`), or `null`
 * for anything else — shown as it came.
 */
export function radioStationErrorKey(err: unknown): string | null {
  const message = String(err);
  const known: [string, string][] = [
    ["radio: not an http(s) url", "webRadio.addStation.errors.notHttp"],
    ["radio: offline", "webRadio.addStation.errors.offline"],
    ["radio: unreachable", "webRadio.addStation.errors.unreachable"],
    ["radio: not an audio stream", "webRadio.addStation.errors.notAudio"],
    ["radio: hls", "webRadio.addStation.errors.hls"],
    ["radio: no stream", "webRadio.addStation.errors.noStream"],
  ];
  return known.find(([prefix]) => message.includes(prefix))?.[1] ?? null;
}

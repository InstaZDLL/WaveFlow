import { useCallback, useSyncExternalStore } from "react";
import { listen } from "@tauri-apps/api/event";
import { usePlayer } from "./usePlayer";
import { isRadioTrack } from "../lib/playerSources";
import { playerPlayUrl } from "../lib/tauri/player";

/** Wire shape of `player:radio-timeshift` (`audio::live::TimeshiftPayload`). */
interface TimeshiftPayload {
  track_id: number;
  behind_ms: number;
  capacity_ms: number | null;
}

/** Under this, the listener is live: a station keeps a few seconds of
 *  slack ahead of everyone, and a network hiccup moves it by a second. */
export const LIVE_THRESHOLD_MS = 3000;

// The last report, kept for the whole webview: a view mounted after it —
// a layout swapped, the overflow menu opened — reads it at once rather than
// waiting for the backend to say it again (it does every five seconds).
let lastReport: TimeshiftPayload | null = null;
const subscribers = new Set<() => void>();
let listening = false;

function startListening() {
  if (listening) return;
  listening = true;
  listen<TimeshiftPayload>("player:radio-timeshift", (e) => {
    lastReport = e.payload;
    subscribers.forEach((notify) => notify());
  }).catch((err) => {
    listening = false;
    console.error("[useRadioTimeshift] listen failed", err);
  });
}

function subscribe(notify: () => void) {
  startListening();
  subscribers.add(notify);
  return () => {
    subscribers.delete(notify);
  };
}

function snapshot() {
  return lastReport;
}

export interface RadioTimeshift {
  /** Whether a live station is what plays now. */
  isRadio: boolean;
  /** How far behind live the listener is, 0 when live. */
  behindMs: number;
  /** How far behind they can fall before the oldest audio goes. */
  capacityMs: number | null;
  /** `behindMs` past {@link LIVE_THRESHOLD_MS}. */
  isBehind: boolean;
  /** Play the station again from its live edge. */
  goLive: () => Promise<void>;
}

/**
 * The time-shift state of the station playing now. A paused station keeps
 * arriving into the backend's buffer, so playing again resumes behind live;
 * the backend says by how much on `player:radio-timeshift` while it plays.
 * Going back to live is playing the station again — a new load, which
 * drops that buffer.
 */
export function useRadioTimeshift(): RadioTimeshift {
  const { currentTrack, currentRadioStation } = usePlayer();
  const isRadio = isRadioTrack(currentTrack);
  const trackId = currentTrack?.id ?? null;
  const report = useSyncExternalStore(subscribe, snapshot);

  // Only what was said about the station playing now counts: a station
  // played again starts live, whatever the previous one reported.
  const reported = isRadio && report?.track_id === trackId ? report : null;
  const behindMs = reported?.behind_ms ?? 0;

  const goLive = useCallback(async () => {
    const station = currentRadioStation;
    if (!station || !station.id.startsWith("url:")) return;
    await playerPlayUrl({
      url: station.id.slice("url:".length),
      title: station.title,
      artist: station.artist,
      artworkUrl: station.artworkUrl ?? undefined,
    });
  }, [currentRadioStation]);

  return {
    isRadio,
    behindMs,
    capacityMs: reported?.capacity_ms ?? null,
    isBehind: behindMs >= LIVE_THRESHOLD_MS,
    goLive,
  };
}

import { useCallback, useEffect, useState } from "react";
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
  const [state, setState] = useState<{
    trackId: number;
    behindMs: number;
    capacityMs: number | null;
  } | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    (async () => {
      try {
        const off = await listen<TimeshiftPayload>(
          "player:radio-timeshift",
          (e) => {
            setState({
              trackId: e.payload.track_id,
              behindMs: e.payload.behind_ms,
              capacityMs: e.payload.capacity_ms,
            });
          },
        );
        if (cancelled) off();
        else unlisten = off;
      } catch (err) {
        console.error("[useRadioTimeshift] listen failed", err);
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Only what was said about the station playing now counts: a station
  // played again starts live, whatever the previous one reported.
  const reported = isRadio && state?.trackId === trackId ? state : null;
  const behindMs = reported?.behindMs ?? 0;

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
    capacityMs: reported?.capacityMs ?? null,
    isBehind: behindMs >= LIVE_THRESHOLD_MS,
    goLive,
  };
}

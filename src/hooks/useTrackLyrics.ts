import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { usePlayer } from "./usePlayer";
import { isRadioTrack } from "../lib/playerSources";
import { pickFile } from "../lib/tauri/dialog";
import {
  clearLyrics,
  fetchLyrics,
  fetchRadioLyrics,
  findActiveLineIndex,
  findActiveWordIndex,
  importLrcFile,
  parseLyrics,
  refetchLyrics,
  type LyricsLine,
  type LyricsPayload,
  type LyricsProvider,
} from "../lib/tauri/lyrics";

/**
 * Owns the full lyrics lifecycle for the currently-playing track: the
 * three-tier `fetch_lyrics` resolution (cache → embedded tag → LRCLIB),
 * LRC parsing, synced active-line / active-word tracking, and the
 * user-triggered import / refetch / clear mutations — every one of the
 * mid-flight staleness guards that grew on `LyricsPanel` over time.
 *
 * Both the right-edge `LyricsPanel` and the immersive view consume this
 * hook so the merged immersive layout doesn't double-implement (or
 * double-fetch through a second code path) the lyrics state. The hook
 * keys on the live `currentTrack` from `PlayerContext`, so two mounted
 * consumers stay in lock-step on the same track; the backend caches the
 * `fetch_lyrics` result, so the (rare) case of both the side panel and
 * the immersive view being open resolves the second call from cache.
 *
 * Auto-scroll is deliberately NOT here — each consumer keeps its own
 * `scrollIntoView` against its own line-ref array (the side panel and
 * the immersive scroller scroll independently), driven off the shared
 * `activeIndex` this hook exposes.
 */
export interface TrackLyrics {
  payload: LyricsPayload | null;
  isFetching: boolean;
  error: string | null;
  /** Parsed lines (empty when plain / no payload). */
  lrcLines: LyricsLine[];
  /** True only for non-radio synced LRC — drives the karaoke highlight. */
  isSynced: boolean;
  /** Radio: timestamp-stripped static read (`null` for library tracks). */
  radioPlainText: string | null;
  /** True when the current track is a live Web Radio session. */
  isRadio: boolean;
  /** Active synced line index (`-1` when none / not synced). */
  activeIndex: number;
  /** Active word index inside the active line (`-1` when no word stamps). */
  activeWordIndex: number;
  /** The active line object, or `undefined`. */
  activeLine: LyricsLine | undefined;
  /** Pick a sidecar lyrics file and attach it to the current track. */
  importLyrics: () => Promise<void>;
  /** Re-query lyrics (full waterfall when `provider` omitted, else that
   *  source only). */
  refetch: (provider?: LyricsProvider) => Promise<void>;
  /** Drop the cached lyrics row for the current track. */
  clear: () => Promise<void>;
  /** Seek playback to a synced line's timestamp. */
  seekToLine: (line: LyricsLine) => void;
  /** Replace the payload from an external source (e.g. the editor). */
  applyPayload: (next: LyricsPayload | null) => void;
}

export function useTrackLyrics(): TrackLyrics {
  const { t } = useTranslation();
  const { currentTrack, positionMs, seek } = usePlayer();

  const [payload, setPayload] = useState<LyricsPayload | null>(null);
  const [isFetching, setIsFetching] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const trackId = currentTrack?.id ?? null;

  // Web Radio has no library row: its lyrics are fetched by (artist,
  // title) parsed from the ICY title, not by track_id. The sentinel id
  // stays constant for the whole stream session while the song changes,
  // so the fetch effect keys on title + artist (not just trackId) to
  // re-query on each new song. Synced lyrics are rendered statically for
  // radio — the live stream position can't align to a song joined
  // mid-play — and the library-row mutation actions (edit / import /
  // refetch / clear) are hidden by the consumers.
  const isRadio = isRadioTrack(currentTrack);
  const radioArtist = isRadio ? (currentTrack?.artist_name ?? null) : null;
  const radioTitle = isRadio ? (currentTrack?.title ?? null) : null;

  // Live mirror of `trackId` so async handlers can detect when the user
  // switched tracks during an `await` — without it the closure carries
  // whatever `trackId` was current at call time and a stale
  // `refetchLyrics` / `importLrcFile` response would happily overwrite
  // the new track's payload after the user moved on.
  const trackIdRef = useRef<number | null>(trackId);
  useEffect(() => {
    trackIdRef.current = trackId;
  }, [trackId]);

  // Previous `isRadio` so the fetch effect can tell a context switch
  // (library ↔ radio) from a same-context track change.
  const prevIsRadioRef = useRef(isRadio);

  // ── Fetch when the focused track changes ─────────────────────────
  useEffect(() => {
    if (trackId == null) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setPayload(null);
      setError(null);
      // Clear the spinner too — without this a fetch in flight when the
      // track drops to null leaves `isFetching` stuck true.
      setIsFetching(false);
      prevIsRadioRef.current = isRadio;
      return;
    }
    let cancelled = false;
    // Drop the previous payload up front on any transition that involves
    // radio — entering radio (library→radio), leaving it (radio→library),
    // or a new song on the same station (radio→radio, where the sentinel
    // trackId is unchanged so the swap-on-resolve below wouldn't fire).
    // Without this the previous lyrics linger under the new identity for
    // the duration of the fetch. Library→library is deliberately exempt:
    // it keeps the swap-on-resolve so a fast cache hit doesn't flash an
    // intermediate "loading" state.
    const wasRadio = prevIsRadioRef.current;
    prevIsRadioRef.current = isRadio;
    if (isRadio || wasRadio) {
      setPayload(null);
    }
    setIsFetching(true);
    setError(null);
    // Radio: query by artist + title (no library row). A radio session
    // with no parsed song yet (favicon-only, pre-ICY) has nothing to
    // search — resolve to null so the consumer shows "not found" instead
    // of firing a blank query.
    const request = isRadio
      ? radioArtist && radioTitle
        ? fetchRadioLyrics(radioArtist, radioTitle, trackId)
        : Promise.resolve<LyricsPayload | null>(null)
      : fetchLyrics(trackId);
    request
      .then((p) => {
        if (cancelled) return;
        setPayload(p);
      })
      .catch((err) => {
        if (cancelled) return;
        console.error("[useTrackLyrics] fetch failed", err);
        setError(String(err));
      })
      .finally(() => {
        if (!cancelled) setIsFetching(false);
      });
    return () => {
      cancelled = true;
    };
  }, [trackId, isRadio, radioArtist, radioTitle]);

  // ── Parse lyrics once per content change ─────────────────────────
  const lrcLines = useMemo<LyricsLine[]>(() => {
    if (!payload) return [];
    return parseLyrics(payload.content, payload.format);
  }, [payload]);

  // Radio is always rendered statically (no karaoke scroll), even when
  // the fetched content is synced LRC — the stream position is "seconds
  // since I tuned in", not "seconds into the song", so a highlight would
  // be wrong.
  const isSynced = !isRadio && lrcLines.length > 0;

  // For radio, strip the LRC timestamps for a clean static read: reuse
  // the parsed lines' text, or fall back to the raw content when it was
  // already plain.
  const radioPlainText = useMemo<string | null>(() => {
    if (!isRadio || !payload) return null;
    if (lrcLines.length > 0) return lrcLines.map((l) => l.text).join("\n");
    return payload.content;
  }, [isRadio, payload, lrcLines]);

  // ── Active-line tracking (auto-scroll lives in each consumer) ─────
  const [activeIndex, setActiveIndex] = useState(-1);
  useEffect(() => {
    if (!isSynced) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setActiveIndex(-1);
      return;
    }
    const idx = findActiveLineIndex(
      lrcLines,
      positionMs,
      Math.max(activeIndex, 0),
    );
    if (idx !== activeIndex) {
      setActiveIndex(idx);
    }
  }, [positionMs, lrcLines, isSynced, activeIndex]);

  // Active word inside the active line — only computed when the line
  // carries `words[]` so plain LRC stays cheap.
  const activeLine = activeIndex >= 0 ? lrcLines[activeIndex] : undefined;
  const activeWordIndex = useMemo(() => {
    if (!activeLine?.words || activeLine.words.length === 0) return -1;
    return findActiveWordIndex(activeLine.words, positionMs);
  }, [activeLine, positionMs]);

  // ── Actions ──────────────────────────────────────────────────────
  const importLyrics = useCallback(async () => {
    if (trackId == null) return;
    // Capture the requested track at the call site: the user can switch
    // tracks during the file picker (which can sit on screen for a
    // while) and again during `importLrcFile`'s disk + DB work. Without
    // the guard a stale import would clobber the new track's payload.
    //
    // We deliberately let `importLrcFile` run to completion even when
    // the user has switched away: the intent was to attach this LRC to
    // the captured track, and the call writes straight to that track's
    // DB row — cancelling the write would lose work. Only UI updates
    // skip when stale.
    const requestedTrackId = trackId;
    try {
      const path = await pickFile(
        ["lrc", "elrc", "ttml", "xml", "txt"],
        t("lyrics.importTitle"),
      );
      if (!path) return;
      const next = await importLrcFile(requestedTrackId, path);
      if (requestedTrackId !== trackIdRef.current) return;
      setPayload(next);
      // Drop any error left from a prior failed fetch — otherwise the
      // error-vs-notFound conditional in the consumer would mask the
      // freshly imported lyrics behind the stale error state.
      setError(null);
    } catch (err) {
      console.error("[useTrackLyrics] import failed", err);
      if (requestedTrackId !== trackIdRef.current) return;
      setError(String(err));
    }
  }, [trackId, t]);

  const refetch = useCallback(
    async (provider?: LyricsProvider) => {
      if (trackId == null) return;
      // Capture the requested track so we can detect a mid-flight switch
      // by comparing against the live `trackIdRef` when the await
      // resolves. Without this a refetch on track A that outlives the
      // user's switch to track B would land its result into B's payload.
      const requestedTrackId = trackId;
      try {
        // `refetchLyrics` drops the cache row + re-queries in one Tauri
        // call. `provider = undefined` re-runs the full waterfall;
        // `provider` set queries ONLY that source, bypassing local tiers
        // — the path the user takes when the auto-fetch cached a
        // low-quality hit and they want a different source (issue #284).
        setIsFetching(true);
        const next = await refetchLyrics(requestedTrackId, provider);
        if (requestedTrackId !== trackIdRef.current) return;
        setPayload(next);
        setError(null);
      } catch (err) {
        console.error("[useTrackLyrics] refetch failed", err);
        // Don't surface an error for a track the user no longer cares
        // about — the new track's fetch effect handles its own state.
        if (requestedTrackId !== trackIdRef.current) return;
        setError(String(err));
      } finally {
        // Only clear the spinner when we're still on the same track.
        // After a switch the fetch effect already flipped `isFetching`
        // to `true` for its own request and our clear would race it.
        if (requestedTrackId === trackIdRef.current) {
          setIsFetching(false);
        }
      }
    },
    [trackId],
  );

  const clear = useCallback(async () => {
    if (trackId == null) return;
    // Same staleness guard as importLyrics / refetch: a track switch
    // during the await would otherwise wipe the NEW track's payload.
    const requestedTrackId = trackId;
    try {
      await clearLyrics(requestedTrackId);
      if (requestedTrackId !== trackIdRef.current) return;
      setPayload(null);
      // Drop any stale error too so the empty state isn't masked.
      setError(null);
    } catch (err) {
      console.error("[useTrackLyrics] clear failed", err);
    }
  }, [trackId]);

  const seekToLine = useCallback(
    (line: LyricsLine) => {
      seek(line.timeMs).catch(() => {});
    },
    [seek],
  );

  const applyPayload = useCallback((next: LyricsPayload | null) => {
    setPayload(next);
    // Clear any stale error so freshly applied external lyrics (e.g. from
    // the editor) aren't masked behind a prior fetch error — mirrors the
    // cleanup in importLyrics / refetch.
    if (next != null) setError(null);
  }, []);

  return {
    payload,
    isFetching,
    error,
    lrcLines,
    isSynced,
    radioPlainText,
    isRadio,
    activeIndex,
    activeWordIndex,
    activeLine,
    importLyrics,
    refetch,
    clear,
    seekToLine,
    applyPayload,
  };
}

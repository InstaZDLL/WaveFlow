import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { usePlayer } from "./usePlayer";
import { usePlayerPosition } from "./usePlayerPosition";
import { useEstimatedKaraoke } from "./useEstimatedKaraoke";
import { estimateLineWords } from "../lib/lyricsWordEstimate";
import { isRadioTrack, isRemoteTrack } from "../lib/playerSources";
import { pickFile } from "../lib/tauri/dialog";
import { remoteGetPlayQueue } from "../lib/tauri/remoteServer";
import {
  clearLyrics,
  fetchLyrics,
  fetchRadioLyrics,
  fetchRemoteLyrics,
  findActiveLineIndex,
  findActiveWordIndex,
  findOverlappingLineIndex,
  findInterludes,
  importLrcFile,
  lyricsExcludedGenre,
  isUntimedLrc,
  parseLyrics,
  refetchLyrics,
  type LyricsInterlude,
  type LyricsLine,
  type LyricsWord,
  type LyricsPayload,
  type LyricsProvider,
  type PluginLyricsProvider,
} from "../lib/tauri/lyrics";

const NO_LINES: LyricsLine[] = [];

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
/** An interlude being played through, with how far into it (0 to 1). */
export interface ActiveInterlude extends LyricsInterlude {
  progress: number;
}

export interface TrackLyrics {
  payload: LyricsPayload | null;
  isFetching: boolean;
  error: string | null;
  /** When the lookup came back empty because the track's genre is
   *  excluded from the online search (#721): that genre, as tagged. The
   *  panels then say nothing was searched, not that nothing was found. */
  excludedGenre: string | null;
  /** Parsed lines (empty when plain / no payload). */
  lrcLines: LyricsLine[];
  /** True only for non-radio synced LRC — drives the karaoke highlight. */
  isSynced: boolean;
  /** Radio: timestamp-stripped static read (`null` for library tracks). */
  radioPlainText: string | null;
  /** True when the current track is a live Web Radio session. */
  isRadio: boolean;
  /** True when the current track is a remote-source stream (RFC-005).
   *  Synced lyrics still render (its position aligns), but the library-row
   *  mutations — import / refetch / clear / edit — don't apply. */
  isRemote: boolean;
  /** Active synced line index (`-1` when none / not synced). */
  activeIndex: number;
  /** Active word index inside the active line (`-1` when no word stamps). */
  activeWordIndex: number;
  /** The active line object, or `undefined`. */
  activeLine: LyricsLine | undefined;
  /** Active word of the active line's background vocals (`-1` when none
   *  has started, or the line has none). */
  activeBackgroundWordIndex: number;
  /** A line before the active one still being sung — the other voice
   *  of a duet holding its end — or `-1`. Drawn active beside it; the
   *  scroll keeps following `activeIndex`. */
  overlapIndex: number;
  /** Active word inside the overlapping line (`-1` when none). */
  overlapWordIndex: number;
  /** Active word of the overlapping line's background vocals. */
  overlapBackgroundWordIndex: number;
  /** Every interlude of the synced lyric (empty when not synced). */
  interludes: LyricsInterlude[];
  /** The interlude the position is in, if any. `activeIndex` keeps
   *  pointing at the line before it (`-1` for the intro), so a surface
   *  that shows the interlude also dims that line. */
  activeInterlude: ActiveInterlude | null;
  /** Pick a sidecar lyrics file and attach it to the current track. */
  importLyrics: () => Promise<void>;
  /** Re-query lyrics (full waterfall when `provider` omitted, else that
   *  source only). */
  refetch: (provider?: LyricsProvider | PluginLyricsProvider) => Promise<void>;
  /** Drop the cached lyrics row for the current track. */
  clear: () => Promise<void>;
  /** Seek playback to a synced line's timestamp. */
  seekToLine: (line: LyricsLine) => void;
  /** Replace the payload from an external source (e.g. the editor). */
  applyPayload: (next: LyricsPayload | null) => void;
}

export function useTrackLyrics(): TrackLyrics {
  const { t } = useTranslation();
  const { currentTrack, seek } = usePlayer();
  const positionMs = usePlayerPosition();

  const [payload, setPayload] = useState<LyricsPayload | null>(null);
  const [isFetching, setIsFetching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [excludedGenre, setExcludedGenre] = useState<string | null>(null);
  // Bumped by a refetch. The automatic fetch reads it when it starts and
  // drops its answer if a refetch began in the meantime: otherwise a
  // fetch that started first but finished last would overwrite the
  // refetch's lyrics, or put back an excluded-genre message the refetch
  // has just made untrue. `cancelled` only covers a track change.
  const fetchGenerationRef = useRef(0);

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

  // A remote-source track (RFC-005) also has no library row, but unlike
  // radio it has a stable identity and a known length — so its lyrics are
  // fetched from the server (by the queue's remote id) with an LRCLIB
  // fallback, and rendered synced. The library-row mutations (import /
  // refetch / clear / edit) still don't apply and the consumer hides them.
  const isRemote = isRemoteTrack(currentTrack);
  const remoteArtist = isRemote ? (currentTrack?.artist_name ?? null) : null;
  const remoteTitle = isRemote ? (currentTrack?.title ?? null) : null;
  const remoteDurationMs = isRemote ? (currentTrack?.duration_ms ?? 0) : 0;

  // Live mirror of `trackId` so async handlers can detect when the user
  // switched tracks during an `await` — without it the closure carries
  // whatever `trackId` was current at call time and a stale
  // `refetchLyrics` / `importLrcFile` response would happily overwrite
  // the new track's payload after the user moved on.
  const trackIdRef = useRef<number | null>(trackId);
  useEffect(() => {
    trackIdRef.current = trackId;
  }, [trackId]);

  // Previous streaming state (radio OR remote) so the fetch effect can
  // tell a context switch — into, out of, or between streaming sources —
  // from a same-context library track change.
  const isStream = isRadio || isRemote;
  const prevIsStreamRef = useRef(isStream);

  // ── Fetch when the focused track changes ─────────────────────────
  useEffect(() => {
    if (trackId == null) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setPayload(null);
      setError(null);
      setExcludedGenre(null);
      // Clear the spinner too — without this a fetch in flight when the
      // track drops to null leaves `isFetching` stuck true.
      setIsFetching(false);
      prevIsStreamRef.current = isStream;
      return;
    }
    let cancelled = false;
    // Drop the previous payload up front on any transition that involves a
    // streaming source — entering one (library→radio/remote), leaving it
    // (radio/remote→library), switching between them, or a new song on the
    // same station / remote queue (where the sentinel trackId is unchanged
    // so the swap-on-resolve below wouldn't fire). Without this the previous
    // lyrics linger under the new identity for the duration of the fetch.
    // Library→library is deliberately exempt: it keeps the swap-on-resolve
    // so a fast cache hit doesn't flash an intermediate "loading" state.
    const wasStream = prevIsStreamRef.current;
    prevIsStreamRef.current = isStream;
    if (isStream || wasStream) {
      setPayload(null);
    }
    setIsFetching(true);
    setError(null);
    setExcludedGenre(null);
    const generation = fetchGenerationRef.current;
    const stale = () => cancelled || generation !== fetchGenerationRef.current;
    // Radio: query by artist + title (no library row). A radio session
    // with no parsed song yet (favicon-only, pre-ICY) has nothing to
    // search — resolve to null so the consumer shows "not found" instead
    // of firing a blank query.
    const request = isRadio
      ? radioArtist && radioTitle
        ? fetchRadioLyrics(radioArtist, radioTitle, trackId)
        : Promise.resolve<LyricsPayload | null>(null)
      : isRemote
        ? remoteArtist && remoteTitle
          ? // The lyrics are keyed by the server track id, which lives on
            // the live remote queue's current entry (the synthesized Track
            // only carries the negative sentinel). Resolve it, then fetch.
            remoteGetPlayQueue().then((q) => {
              const remoteId = q?.entries[q.index]?.id ?? null;
              return remoteId
                ? fetchRemoteLyrics(
                    remoteId,
                    remoteArtist,
                    remoteTitle,
                    remoteDurationMs,
                    trackId,
                  )
                : null;
            })
          : Promise.resolve<LyricsPayload | null>(null)
        : fetchLyrics(trackId);
    request
      .then((p) => {
        if (stale()) return;
        setPayload(p);
        // An empty answer for a library track may mean "not searched":
        // ask why before the panel says "not found". A failure here only
        // leaves the generic message.
        if (!isStream && (p == null || p.content.trim() === "")) {
          lyricsExcludedGenre(trackId)
            .then((genre) => {
              if (!stale()) setExcludedGenre(genre);
            })
            .catch((err) =>
              console.error("[useTrackLyrics] excluded genre failed", err),
            );
        }
      })
      .catch((err) => {
        if (stale()) return;
        console.error("[useTrackLyrics] fetch failed", err);
        setError(String(err));
      })
      .finally(() => {
        // A refetch that started meanwhile owns the spinner now.
        if (!stale()) setIsFetching(false);
      });
    return () => {
      cancelled = true;
    };
  }, [
    trackId,
    isRadio,
    radioArtist,
    radioTitle,
    isRemote,
    isStream,
    remoteArtist,
    remoteTitle,
    remoteDurationMs,
  ]);

  // ── Parse lyrics once per content change ─────────────────────────
  const rawLines = useMemo<LyricsLine[]>(() => {
    if (!payload) return [];
    return parseLyrics(payload.content, payload.format);
  }, [payload]);

  // Stamped LRC whose lines all sit on one time is untimed: it is handed
  // out as the plain text it really is, which every surface already
  // renders, rather than as synced lines of which only the last lights.
  const untimed =
    payload != null &&
    (payload.format === "lrc" || payload.format === "enhanced_lrc") &&
    isUntimedLrc(rawLines);
  const parsedLines = untimed ? NO_LINES : rawLines;
  const shownPayload = useMemo<LyricsPayload | null>(
    () =>
      untimed && payload
        ? {
            ...payload,
            content: rawLines.map((line) => line.text).join("\n"),
            format: "plain",
          }
        : payload,
    [untimed, payload, rawLines],
  );

  // Radio is always rendered statically (no karaoke scroll), even when
  // the fetched content is synced LRC — the stream position is "seconds
  // since I tuned in", not "seconds into the song", so a highlight would
  // be wrong.
  const isSynced = !isRadio && parsedLines.length > 0;

  // For radio, strip the LRC timestamps for a clean static read: reuse
  // the parsed lines' text, or fall back to the raw content when it was
  // already plain.
  const radioPlainText = useMemo<string | null>(() => {
    if (!isRadio || !payload) return null;
    if (rawLines.length > 0) return rawLines.map((l) => l.text).join("\n");
    return payload.content;
  }, [isRadio, payload, rawLines]);

  // ── Active-line tracking (auto-scroll lives in each consumer) ─────
  const [activeIndex, setActiveIndex] = useState(-1);
  useEffect(() => {
    if (!isSynced) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setActiveIndex(-1);
      return;
    }
    const idx = findActiveLineIndex(
      parsedLines,
      positionMs,
      Math.max(activeIndex, 0),
    );
    if (idx !== activeIndex) {
      setActiveIndex(idx);
    }
  }, [positionMs, parsedLines, isSynced, activeIndex]);

  // Estimated word timing for line-synced lyrics (#716), opt-in. Only
  // the active line gets words, re-derived each time a line becomes
  // active and never stored, so a lyric the user reads plainly costs
  // nothing and switching the setting off leaves no trace. A line that
  // already carries real word timing is left exactly as it is.
  const estimateWords = useEstimatedKaraoke();
  // The other voice of a duet, while it holds the end of its line. Found
  // on the parsed lines: estimating words never moves a line's bounds.
  const overlapIndex = useMemo(
    () =>
      isSynced && activeIndex >= 0
        ? findOverlappingLineIndex(parsedLines, activeIndex, positionMs)
        : -1,
    [isSynced, parsedLines, activeIndex, positionMs],
  );
  const lrcLines = useMemo<LyricsLine[]>(() => {
    if (!estimateWords || !isSynced || activeIndex < 0) return parsedLines;
    let next: LyricsLine[] | null = null;
    // The overlapping line keeps words too, estimated exactly as when it
    // was active, so handing over does not reshape them mid-sweep.
    for (const index of [activeIndex, overlapIndex]) {
      const line = parsedLines[index];
      if (!line || (line.words?.length ?? 0) > 0) continue;
      const words = estimateLineWords(line, estimateBound(parsedLines, index));
      if (words.length === 0) continue;
      next ??= parsedLines.slice();
      next[index] = { ...line, words };
    }
    return next ?? parsedLines;
  }, [estimateWords, isSynced, activeIndex, overlapIndex, parsedLines]);

  // Active word inside the active line — only computed when the line
  // carries `words[]` so plain LRC stays cheap.
  const activeLine = activeIndex >= 0 ? lrcLines[activeIndex] : undefined;
  const activeWordIndex = useMemo(() => {
    if (!activeLine?.words || activeLine.words.length === 0) return -1;
    return findActiveWordIndex(activeLine.words, positionMs);
  }, [activeLine, positionMs]);

  // Background vocals keep their own clock — they often start before
  // the lead — so their word is found from the position, not borrowed
  // from the lead's index.
  const activeBackgroundWordIndex = useMemo(() => {
    const words = activeLine?.background?.words;
    if (!words || words.length === 0) return -1;
    return findActiveWordIndex(words, positionMs);
  }, [activeLine, positionMs]);

  const overlapLine = overlapIndex >= 0 ? lrcLines[overlapIndex] : undefined;
  const overlapWordIndex = useMemo(
    () => overlapWordAt(overlapLine?.words, positionMs),
    [overlapLine, positionMs],
  );
  const overlapBackgroundWordIndex = useMemo(
    () => overlapWordAt(overlapLine?.background?.words, positionMs),
    [overlapLine, positionMs],
  );

  const interludes = useMemo(
    () => (isSynced ? findInterludes(parsedLines) : []),
    [isSynced, parsedLines],
  );
  const activeInterlude = useMemo<ActiveInterlude | null>(() => {
    const gap = interludes.find(
      (g) => positionMs >= g.startMs && positionMs < g.endMs,
    );
    if (!gap) return null;
    return {
      ...gap,
      progress: (positionMs - gap.startMs) / (gap.endMs - gap.startMs),
    };
  }, [interludes, positionMs]);

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
    async (provider?: LyricsProvider | PluginLyricsProvider) => {
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
        fetchGenerationRef.current += 1;
        const next = await refetchLyrics(requestedTrackId, provider);
        if (requestedTrackId !== trackIdRef.current) return;
        setPayload(next);
        setError(null);
        // A refetch searches whatever the genre, so an empty answer now
        // really is "not found".
        setExcludedGenre(null);
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
    payload: shownPayload,
    isFetching,
    error,
    excludedGenre,
    lrcLines,
    isSynced,
    radioPlainText,
    isRadio,
    isRemote,
    activeIndex,
    activeWordIndex,
    activeLine,
    activeBackgroundWordIndex,
    overlapIndex,
    overlapWordIndex,
    overlapBackgroundWordIndex,
    interludes,
    activeInterlude,
    importLyrics,
    refetch,
    clear,
    seekToLine,
    applyPayload,
  };
}

/**
 * Where a line's estimated words have to fit: the next line's start, or
 * the line's own stated end when it runs past it — the one voice of a
 * duet holding its end while the other starts. Undefined for the last
 * line, as before.
 */
function estimateBound(lines: LyricsLine[], index: number): number | undefined {
  const next = lines[index + 1]?.timeMs;
  // The lead's own end: the line's may include background vocals that
  // outlast it, and those are not the words being estimated.
  const end = lines[index].leadEndMs ?? lines[index].endMs;
  return next !== undefined && end > next ? end : next;
}

/**
 * The word being sung in the overlapping line, or `words.length` once its
 * last word has ended: such a line stays lit while its background vocals
 * carry on, and its own words are then all sung. The active line keeps
 * `findActiveWordIndex`'s rule — its last word is held until the next
 * line takes over — which is what the estimated fill relies on.
 */
function overlapWordAt(
  words: LyricsWord[] | undefined,
  positionMs: number,
): number {
  if (!words || words.length === 0) return -1;
  // `endMs`, not `fillEndMs`: the fill may finish early, but the word is
  // still the one being sung until its own end.
  const end = words[words.length - 1].endMs;
  if (end >= 0 && positionMs >= end) return words.length;
  return findActiveWordIndex(words, positionMs);
}

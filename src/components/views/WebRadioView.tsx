import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Radio, Search, ChevronLeft, Play, Pause } from "lucide-react";

import { playerPlayUrl, playerStop } from "../../lib/tauri/player";
import {
  pluginListEntries,
  pluginResolve,
  pluginStreamUrl,
  type PluginEntry,
  type PluginTrack,
} from "../../lib/tauri/plugins";

const PLUGIN_ID = "web-radio";

/**
 * Web Radio view (Phase 4.c — engine-integrated).
 *
 * Clicking a category fires the plugin's `resolve(query)`; clicking a
 * station resolves the stream URL via `stream-url` and hands it to
 * `player_play_url`. From that point on, the cpal engine drives
 * playback through the same EQ / ReplayGain / WASAPI Exclusive /
 * spectrum / Discord RPC pipeline the local library uses — the radio
 * is a first-class source, not a sandboxed HTML5 audio element.
 *
 * The previous HTML5 `<audio>` stop-gap (Phase 4.b) lived here through
 * one release window before being promoted.
 */
export function WebRadioView() {
  const { t } = useTranslation();
  const [entries, setEntries] = useState<PluginEntry[]>([]);
  const [activeEntry, setActiveEntry] = useState<PluginEntry | null>(null);
  const [tracks, setTracks] = useState<PluginTrack[]>([]);
  const [loading, setLoading] = useState(true);
  const [resolving, setResolving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [searchTerm, setSearchTerm] = useState("");
  const [searchActive, setSearchActive] = useState(false);
  // The PluginTrack.id of the currently-playing station, or null when
  // nothing in this view owns the engine. Driven by user clicks AND
  // by `player:state` events so an external stop (PlayerBar, OS
  // overlay, queue resume) collapses the highlight cleanly.
  const [playingId, setPlayingId] = useState<string | null>(null);
  // Per-async-call request tokens. Each handler increments + captures
  // its value before await; the await's continuation drops itself
  // when the ref has moved on. Two counters because category resolves
  // (openEntry + runSearch) race the same surface, but stream-url
  // clicks are independent — a play click shouldn't invalidate a
  // pending category fetch.
  const resolveReqRef = useRef(0);
  const streamReqRef = useRef(0);

  // Initial fetch of the category list (`.then`-style per the same
  // `react-hooks/set-state-in-effect` constraint that drives
  // PluginsCard.tsx — see that file for the rationale).
  useEffect(() => {
    let cancelled = false;
    pluginListEntries(PLUGIN_ID).then(
      (list) => {
        if (cancelled) return;
        setEntries(list);
        setError(null);
        setLoading(false);
      },
      (e: unknown) => {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : String(e));
        setLoading(false);
      },
    );
    return () => {
      cancelled = true;
    };
  }, []);

  // Track the engine's lifecycle so a stream that dies on its own
  // (server timeout, mid-stream 5xx, user hits Stop on the PlayerBar)
  // un-highlights the row. The engine emits `player:state` with
  // `state: "idle"` whenever the decoder backs out of `play_track`
  // — natural EOF, Stop, or open-failure all converge there.
  useEffect(() => {
    let cancelled = false;
    const unlisten: UnlistenFn[] = [];
    (async () => {
      try {
        const u = await listen<{ state: string }>("player:state", (e) => {
          if (e.payload.state === "idle" || e.payload.state === "ended") {
            setPlayingId(null);
          }
        });
        unlisten.push(u);
      } catch (err) {
        console.error("[WebRadioView] listen setup failed", err);
      }
      if (cancelled) unlisten.forEach((u) => u());
    })();
    return () => {
      cancelled = true;
      unlisten.forEach((u) => u());
    };
  }, []);

  const openEntry = useCallback(async (entry: PluginEntry) => {
    const token = ++resolveReqRef.current;
    setActiveEntry(entry);
    setSearchActive(false);
    setResolving(true);
    setError(null);
    setTracks([]);
    try {
      const list = await pluginResolve(PLUGIN_ID, entry.query);
      // A later openEntry / runSearch ran while we were awaiting —
      // drop this stale list silently so the user's most-recent
      // click wins.
      if (resolveReqRef.current !== token) return;
      setTracks(list);
    } catch (e) {
      if (resolveReqRef.current !== token) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (resolveReqRef.current === token) {
        setResolving(false);
      }
    }
  }, []);

  const runSearch = useCallback(async () => {
    const term = searchTerm.trim();
    if (term.length === 0) return;
    const token = ++resolveReqRef.current;
    setActiveEntry(null);
    setSearchActive(true);
    setResolving(true);
    setError(null);
    setTracks([]);
    try {
      const list = await pluginResolve(PLUGIN_ID, term);
      if (resolveReqRef.current !== token) return;
      setTracks(list);
    } catch (e) {
      if (resolveReqRef.current !== token) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (resolveReqRef.current === token) {
        setResolving(false);
      }
    }
  }, [searchTerm]);

  const backToCategories = useCallback(() => {
    // Invalidate every in-flight async on the way out:
    // - Bumping `resolveReqRef` makes a pending `pluginResolve`
    //   continuation drop itself instead of restoring the
    //   abandoned category's tracks on top of the home view.
    // - Bumping `streamReqRef` does the same for `pluginStreamUrl` /
    //   `playerPlayUrl` so a click on a station + an immediate "back"
    //   can't fire `playerPlayUrl` a moment later under the category
    //   list.
    //
    // We deliberately do NOT call `playerStop` here — going back to
    // the category list shouldn't kill audio (the PlayerBar still
    // owns playback). If the user wants to stop, that's the
    // PlayerBar's job.
    //
    // Clearing `playingId` is the right rollback when the bump above
    // strands an optimistic highlight from a click whose stream-url
    // resolve hadn't landed yet — otherwise the row would stay lit
    // even though `playerPlayUrl` never fired. Trade-off: if a stream
    // is actually playing when the user backs out, the highlight is
    // lost on re-entry until they click again. PlayerBar remains the
    // source of truth for "what's playing".
    resolveReqRef.current += 1;
    streamReqRef.current += 1;
    setActiveEntry(null);
    setSearchActive(false);
    setTracks([]);
    setPlayingId(null);
  }, []);

  const playTrack = useCallback(
    async (track: PluginTrack) => {
      // Toggle off if the user clicks the currently-playing row.
      // `playerStop` will trigger a `player:state` event that
      // collapses `playingId` for us — no need to clear it here.
      if (playingId === track.id) {
        streamReqRef.current += 1;
        try {
          await playerStop();
        } catch (e) {
          setError(e instanceof Error ? e.message : String(e));
        }
        return;
      }
      const token = ++streamReqRef.current;
      setError(null);
      // Optimistically highlight before the await so the row gives
      // immediate feedback. If the stream-url resolve fails or a
      // newer click supersedes us, we roll back in the catch / token
      // guard below.
      setPlayingId(track.id);
      try {
        const url = await pluginStreamUrl(PLUGIN_ID, track.id);
        if (streamReqRef.current !== token) return;
        // Hand off to the cpal engine. `playerPlayUrl` returns the
        // negative sentinel track id — we don't need it here, the
        // PlayerContext picks the metadata up off the
        // `player:radio-metadata` event the decoder emits.
        await playerPlayUrl({
          url,
          title: track.title,
          artist: track.artist,
          artworkUrl: track.artworkUrl ?? undefined,
        });
        // No success-side state update: the engine will emit
        // `player:state -> playing` on its own.
      } catch (e) {
        if (streamReqRef.current !== token) return;
        setError(e instanceof Error ? e.message : String(e));
        // Roll back the optimistic highlight.
        setPlayingId(null);
      }
    },
    [playingId],
  );

  const showCategoryList = activeEntry === null && !searchActive;

  return (
    <div className="flex flex-col h-full">
      <header className="px-6 py-5 border-b border-zinc-200 dark:border-zinc-800 flex items-center gap-3">
        <Radio
          size={28}
          className="text-emerald-500 shrink-0"
          aria-hidden="true"
        />
        <div className="min-w-0 flex-1">
          <h1 className="text-2xl font-bold text-zinc-900 dark:text-white">
            {t("webRadio.title")}
          </h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">
            {t("webRadio.subtitle")}
          </p>
        </div>
      </header>

      <div className="px-6 py-3 flex items-center gap-2 border-b border-zinc-200 dark:border-zinc-800">
        {!showCategoryList && (
          <button
            type="button"
            onClick={backToCategories}
            className="flex items-center gap-1 text-xs text-zinc-600 dark:text-zinc-300 hover:text-zinc-900 dark:hover:text-white"
            aria-label={t("webRadio.backToCategories")}
          >
            <ChevronLeft size={16} aria-hidden="true" />
            <span>{t("webRadio.backToCategories")}</span>
          </button>
        )}
        <div className="flex-1" />
        <div className="relative">
          <Search
            size={14}
            className="absolute left-2.5 top-1/2 -translate-y-1/2 text-zinc-400"
            aria-hidden="true"
          />
          <input
            type="search"
            value={searchTerm}
            onChange={(e) => setSearchTerm(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                void runSearch();
              }
            }}
            placeholder={t("webRadio.searchPlaceholder")}
            className="pl-8 pr-3 py-1.5 text-sm rounded bg-zinc-100 dark:bg-zinc-800 text-zinc-900 dark:text-white border border-zinc-200 dark:border-zinc-700 focus:outline-none focus:ring-2 focus:ring-emerald-500 w-64"
            aria-label={t("webRadio.searchPlaceholder")}
          />
        </div>
      </div>

      {error && (
        <div
          role="alert"
          className="mx-6 mt-3 px-3 py-2 bg-red-50 dark:bg-red-950/30 text-xs text-red-700 dark:text-red-300 border border-red-200 dark:border-red-900 rounded"
        >
          {error}
        </div>
      )}

      <div className="flex-1 overflow-y-auto px-6 py-4">
        {loading ? (
          <div className="text-center text-sm text-zinc-500 dark:text-zinc-400 py-12">
            {t("webRadio.loading")}
          </div>
        ) : showCategoryList ? (
          <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-4 gap-3">
            {entries.map((entry) => (
              <button
                key={entry.query}
                type="button"
                onClick={() => {
                  void openEntry(entry);
                }}
                className="text-left px-4 py-6 rounded-xl bg-zinc-100 dark:bg-zinc-800 hover:bg-emerald-50 dark:hover:bg-emerald-950/30 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
              >
                <div className="text-base font-medium text-zinc-900 dark:text-white">
                  {entry.label}
                </div>
              </button>
            ))}
          </div>
        ) : resolving ? (
          <div className="text-center text-sm text-zinc-500 dark:text-zinc-400 py-12">
            {t("webRadio.resolving")}
          </div>
        ) : tracks.length === 0 ? (
          <div className="text-center text-sm text-zinc-500 dark:text-zinc-400 py-12">
            {t("webRadio.emptyResults")}
          </div>
        ) : (
          <ul className="divide-y divide-zinc-200 dark:divide-zinc-800">
            {tracks.map((track) => {
              const isPlaying = playingId === track.id;
              return (
                <li key={track.id} className="py-2">
                  <button
                    type="button"
                    onClick={() => {
                      void playTrack(track);
                    }}
                    className="w-full text-left px-2 py-1.5 rounded hover:bg-zinc-100 dark:hover:bg-zinc-800 flex items-center gap-3 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
                  >
                    <span
                      className={`w-8 h-8 rounded flex items-center justify-center shrink-0 ${
                        isPlaying
                          ? "bg-emerald-500 text-white"
                          : "bg-zinc-200 dark:bg-zinc-700 text-zinc-600 dark:text-zinc-300"
                      }`}
                      aria-hidden="true"
                    >
                      {isPlaying ? <Pause size={14} /> : <Play size={14} />}
                    </span>
                    <span className="min-w-0 flex-1">
                      <span className="block text-sm font-medium text-zinc-900 dark:text-white truncate">
                        {track.title}
                      </span>
                      <span className="block text-xs text-zinc-500 dark:text-zinc-400 truncate">
                        {track.artist}
                        {track.album ? ` · ${track.album}` : ""}
                      </span>
                    </span>
                    <span className="text-[10px] uppercase font-semibold text-emerald-600 dark:text-emerald-400 shrink-0">
                      {track.durationMs === 0
                        ? t("webRadio.live")
                        : `${Math.round(track.durationMs / 1000)}s`}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}

import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Radio, Search, ChevronLeft, Play, Pause } from "lucide-react";

import {
  pluginListEntries,
  pluginResolve,
  pluginStreamUrl,
  type PluginEntry,
  type PluginTrack,
} from "../../lib/tauri/plugins";

const PLUGIN_ID = "web-radio";

/**
 * Web Radio view (Phase 4.b). The category list shows what the
 * plugin's `list-entries` returned; clicking a category fires
 * `resolve(query)` and renders the resulting stations. Clicking a
 * station fires `stream-url` and pipes the URL into an inline
 * `<audio>` element.
 *
 * Why HTML5 `<audio>` instead of the Rust audio engine: the engine
 * is fed by symphonia from local file readers, and bridging an
 * HTTP stream through it (chunked download → ring buffer → cpal)
 * is its own non-trivial work item — punted to v1.6. The dedicated
 * `<audio>` here keeps Web Radio shippable + lets the user pick a
 * station today without competing with the local-library player
 * for the cpal stream.
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
  const [playingId, setPlayingId] = useState<string | null>(null);
  const [playingUrl, setPlayingUrl] = useState<string | null>(null);
  const [playingTitle, setPlayingTitle] = useState<string | null>(null);
  const audioRef = useRef<HTMLAudioElement | null>(null);
  // Per-async-call request tokens. Each handler increments + captures
  // its value before await; the await's continuation drops itself
  // when the ref has moved on. Two counters because category
  // resolves (openEntry + runSearch) race the same surface, but
  // stream-url clicks are independent — a play click shouldn't
  // invalidate a pending category fetch.
  const resolveReqRef = useRef(0);
  const streamReqRef = useRef(0);
  // Pending `setTimeout` id for the `play()` deferral. Cleared on
  // unmount + before scheduling a new one so the callback can never
  // fire against an unmounted audio element.
  const playTimerRef = useRef<number | null>(null);

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
    // Same token guard as openEntry — search + category clicks
    // race the same `resolveReqRef`, so a search fired mid-resolve
    // wins and vice-versa.
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
    // - Bumping `streamReqRef` does the same for `pluginStreamUrl`
    //   so a click on a station + an immediate "back" can't
    //   surface audio under the category list a moment later.
    // - Clearing the deferred `play()` timer + pausing the audio
    //   element close any imperative side-effect already queued.
    resolveReqRef.current += 1;
    streamReqRef.current += 1;
    if (playTimerRef.current !== null) {
      window.clearTimeout(playTimerRef.current);
      playTimerRef.current = null;
    }
    audioRef.current?.pause();
    setActiveEntry(null);
    setSearchActive(false);
    setTracks([]);
    setPlayingId(null);
    setPlayingUrl(null);
  }, []);

  const playTrack = useCallback(
    async (track: PluginTrack) => {
      // Toggle off if the user clicks the currently-playing row.
      // Same belt-and-braces as `backToCategories`: bump the
      // stream counter + drop any pending deferred `play()` so a
      // stale stream-url that's about to land can't restart the
      // station the user just asked to stop.
      if (playingId === track.id) {
        streamReqRef.current += 1;
        if (playTimerRef.current !== null) {
          window.clearTimeout(playTimerRef.current);
          playTimerRef.current = null;
        }
        audioRef.current?.pause();
        setPlayingId(null);
        setPlayingUrl(null);
        return;
      }
      const token = ++streamReqRef.current;
      setError(null);
      try {
        const url = await pluginStreamUrl(PLUGIN_ID, track.id);
        // Drop the stale URL silently if another track was
        // clicked while we awaited — without this guard the older
        // station can land state + start playing on top of the
        // newer one.
        if (streamReqRef.current !== token) return;
        // Pause the current source BEFORE the src rebind so the
        // browser doesn't emit "The play() request was interrupted
        // by a new load request" — that AbortError fires when the
        // user clicks a different station while the previous one
        // is still resolving its load. Pausing first cancels the
        // in-flight load cleanly.
        if (audioRef.current) {
          audioRef.current.pause();
        }
        setPlayingUrl(url);
        setPlayingId(track.id);
        setPlayingTitle(`${track.title} — ${track.artist}`);
        // Wait a tick for the `<audio src=...>` rebind, then play.
        // The pending timer id rides on `playTimerRef` so the
        // unmount cleanup can cancel it AND a back-to-back
        // playTrack call cancels its predecessor before scheduling
        // a fresh one.
        if (playTimerRef.current !== null) {
          window.clearTimeout(playTimerRef.current);
        }
        playTimerRef.current = window.setTimeout(() => {
          playTimerRef.current = null;
          if (streamReqRef.current !== token) return;
          audioRef.current?.play().catch((e: unknown) => {
            if (streamReqRef.current !== token) return;
            // Swallow the AbortError that still surfaces if the
            // user clicked twice fast enough to race the
            // setTimeout — it's benign and gone by the next click.
            if (e instanceof DOMException && e.name === "AbortError") {
              return;
            }
            setError(e instanceof Error ? e.message : String(e));
            setPlayingId(null);
          });
        }, 0);
      } catch (e) {
        if (streamReqRef.current !== token) return;
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [playingId],
  );

  // Cancel any pending `play()` timer when the component unmounts
  // so its callback can't fire against a detached audio element.
  useEffect(
    () => () => {
      if (playTimerRef.current !== null) {
        window.clearTimeout(playTimerRef.current);
        playTimerRef.current = null;
      }
    },
    [],
  );

  // Sync component state with the audio element's `ended` / `error`
  // events so a stream that drops on its own (server timeout, 5xx
  // mid-stream) collapses the highlighted row + the now-playing
  // strip rather than leaving them claiming a station is playing.
  // We don't subscribe to `play` / `pause` because our `playTrack`
  // function already owns those transitions imperatively, and
  // listening would race our own state updates during track swaps.
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    const clear = () => {
      setPlayingId(null);
      setPlayingUrl(null);
    };
    audio.addEventListener("ended", clear);
    audio.addEventListener("error", clear);
    return () => {
      audio.removeEventListener("ended", clear);
      audio.removeEventListener("error", clear);
    };
  }, [playingUrl]);

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

      {playingUrl && (
        <div
          aria-label={t("webRadio.nowPlaying")}
          className="px-6 py-3 border-t border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 flex items-center gap-3"
        >
          <Radio
            size={18}
            className="text-emerald-500 shrink-0"
            aria-hidden="true"
          />
          <span className="text-xs text-zinc-700 dark:text-zinc-200 truncate flex-1">
            {playingTitle ?? ""}
          </span>
          {/* No `controls` attribute: row clicks own the play /
              pause transitions and the component state would
              de-sync with the native button. Volume falls back to
              the OS mixer; Phase 4.c routes Web Radio through the
              cpal engine where volume + the EQ + the rest of the
              player surface apply for real. */}
          <audio
            ref={audioRef}
            src={playingUrl}
            preload="none"
            className="hidden"
          />
        </div>
      )}
    </div>
  );
}

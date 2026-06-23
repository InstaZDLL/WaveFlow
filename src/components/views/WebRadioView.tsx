import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  Radio,
  Search,
  ChevronLeft,
  Play,
  Pause,
  Loader2,
  Puzzle,
  Star,
} from "lucide-react";

import { playerPlayUrl, playerStop } from "../../lib/tauri/player";
import {
  pluginListEntries,
  pluginResolve,
  pluginStreamUrl,
  getPluginFavorites,
  setPluginFavorites,
  type PluginEntry,
  type PluginTrack,
  type PluginFavorite,
} from "../../lib/tauri/plugins";
import { usePluginAvailability } from "../../hooks/usePluginAvailability";
import { useProfile } from "../../hooks/useProfile";

const PLUGIN_ID = "web-radio";

/// Recognise the backend errors that mean "the plugin host refused
/// to invoke web-radio" — disabled toggle, manifest missing after
/// uninstall, runtime drift. Anything else is treated as a real
/// runtime failure and surfaced verbatim so an upstream bug is
/// debuggable, not hidden behind a friendly empty state.
function isPluginUnavailableError(message: string | null): boolean {
  if (!message) return false;
  return (
    message.includes("plugin disabled") ||
    message.includes("plugin not installed") ||
    message.includes("manifest id mismatch")
  );
}

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
  const pluginAvailable = usePluginAvailability(PLUGIN_ID);
  const { activeProfile } = useProfile();
  const [entries, setEntries] = useState<PluginEntry[]>([]);
  const [activeEntry, setActiveEntry] = useState<PluginEntry | null>(null);
  const [tracks, setTracks] = useState<PluginTrack[]>([]);
  const [loading, setLoading] = useState(true);
  const [resolving, setResolving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [searchTerm, setSearchTerm] = useState("");
  const [searchActive, setSearchActive] = useState(false);
  // Per-profile saved stations (issue #289). Loaded from the backend
  // on mount + on profile switch; the "Favorites" pseudo-category
  // replays them with zero network (the stream URL rides inside
  // `track.id` as `url:<stream>`).
  const [favorites, setFavorites] = useState<PluginFavorite[]>([]);
  const [favoritesActive, setFavoritesActive] = useState(false);
  // The PluginTrack.id of the currently-playing station, or null when
  // nothing in this view owns the engine. Driven by `playerPlayUrl`
  // success AND by `player:state` events so an external stop
  // (PlayerBar, OS overlay, queue resume) collapses the highlight
  // cleanly.
  const [playingId, setPlayingId] = useState<string | null>(null);
  // The PluginTrack.id we are currently resolving the stream-url +
  // dispatching `playerPlayUrl` for. Drives the per-row Loader2
  // spinner. Kept separate from `playingId` so an in-flight click
  // never paints a Pause icon — which would invite the user to
  // re-click and accidentally toggle the request off via the
  // `playingId === track.id` branch before the audio has even
  // started.
  const [loadingId, setLoadingId] = useState<string | null>(null);
  // Per-async-call request tokens. Each handler increments + captures
  // its value before await; the await's continuation drops itself
  // when the ref has moved on. Two counters because category resolves
  // (openEntry + runSearch) race the same surface, but stream-url
  // clicks are independent — a play click shouldn't invalidate a
  // pending category fetch.
  const resolveReqRef = useRef(0);
  const streamReqRef = useRef(0);
  // Synchronous mirror of `favorites` so back-to-back toggles compute
  // their next list from the latest value even before React commits a
  // re-render (a rapid double-click on two different stations would
  // otherwise both read the same stale `favorites` and the second
  // would drop the first's change).
  const favoritesRef = useRef<PluginFavorite[]>([]);
  // Serialises optimistic backend writes. Each toggle chains onto the
  // previous write so the snapshots land in click order — a stale
  // (earlier) list can never resolve last and clobber a newer one.
  const writeChainRef = useRef<Promise<unknown>>(Promise.resolve());
  // Monotonic toggle id. A failed write only re-syncs from the server
  // when it's still the most-recent toggle — otherwise an earlier
  // failure's authoritative re-fetch would clobber a newer optimistic
  // state that a later (successful) write already persisted.
  const favoriteSeqRef = useRef(0);
  // Always-current active profile id. A queued favorites write must
  // NOT land after a profile switch — `set_plugin_favorites` resolves
  // `require_profile_pool()` at execution time, so a stale write would
  // persist the old profile's list into the newly-active profile.
  const profileIdRef = useRef(activeProfile?.id);

  // Fetch the category list whenever the plugin becomes available.
  //
  // Re-running on `pluginAvailable` matters because a user can land
  // on this view while the plugin is disabled (initial fetch fails
  // with `"plugin disabled"` → error parked in `error`), then
  // re-enable it from Settings → Plugins. With an empty deps array
  // the view would silently leave the stale error + empty `entries`
  // forever after re-enable. We also reset all fetched state when
  // the plugin flips OFF so a re-enable doesn't briefly flash the
  // previous category list / track view before the fresh fetch
  // lands.
  //
  // `.then`-style per the same `react-hooks/set-state-in-effect`
  // constraint that drives PluginsCard.tsx — see that file for the
  // rationale.
  useEffect(() => {
    if (!pluginAvailable) {
      // React 19 batches these six setState calls into a single
      // re-render, so the cascading-renders concern this rule
      // guards against does not apply. The reset is the cheapest
      // way to keep the view honest when the plugin flips OFF:
      // a stale `activeEntry` / search result would otherwise
      // flash for one tick on re-enable while the fresh fetch
      // is in flight. A `useReducer` with a RESET action would
      // sidestep the rule but adds more weight than this earns.
      /* eslint-disable-next-line react-hooks/set-state-in-effect --
         intentional batched reset on plugin-unavailability */
      setEntries([]);
      setTracks([]);
      setActiveEntry(null);
      setSearchActive(false);
      setFavoritesActive(false);
      setFavorites([]);
      favoritesRef.current = [];
      setError(null);
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
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
  }, [pluginAvailable]);

  // Single writer for the favorites list: keeps the synchronous ref
  // and the rendered state in lockstep so compute-from-ref and
  // display-from-state never diverge. Declared above the load effect
  // so that effect can list it as a dependency without tripping the
  // const TDZ.
  const applyFavorites = useCallback((list: PluginFavorite[]) => {
    favoritesRef.current = list;
    setFavorites(list);
  }, []);

  // Keep the profile-id ref in lockstep with the active profile so a
  // write queued before a switch can detect that it's now stale.
  useEffect(() => {
    profileIdRef.current = activeProfile?.id;
  }, [activeProfile?.id]);

  // Load the active profile's favorites. Re-runs on profile switch
  // (`activeProfile?.id`) so the saved-stations list follows the
  // profile, like liked tracks. Failures are non-fatal — a profile
  // with no favorites is the common case and a backend hiccup just
  // leaves the list empty rather than blocking the whole view.
  useEffect(() => {
    if (!pluginAvailable) return;
    let cancelled = false;
    // Clear before the per-profile reload so a profile switch never
    // flashes the previous profile's stars while the new list is in
    // flight. On first mount the list is already empty so there's no
    // flicker; React batches this with the async result anyway.
    /* eslint-disable-next-line react-hooks/set-state-in-effect --
       intentional reset before per-profile favorites reload */
    setFavorites([]);
    favoritesRef.current = [];
    getPluginFavorites(PLUGIN_ID).then(
      (list) => {
        if (!cancelled) applyFavorites(list);
      },
      (err: unknown) => {
        if (!cancelled) {
          console.warn("[WebRadioView] favorites load failed", err);
        }
      },
    );
    return () => {
      cancelled = true;
    };
  }, [pluginAvailable, activeProfile?.id, applyFavorites]);

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
            setLoadingId(null);
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
    setFavoritesActive(false);
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
    // Leaving the Favorites view: `displayTracks` prioritises
    // `favoritesActive`, so without this the search results in
    // `tracks` would stay hidden behind the favorites list.
    setFavoritesActive(false);
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

  const isFavorite = useCallback(
    (id: string) => favorites.some((f) => f.id === id),
    [favorites],
  );

  // Optimistic toggle. Computes `next` from the synchronous ref (not
  // the possibly-stale `favorites` closure) and chains the backend
  // write onto `writeChainRef` so concurrent toggles persist in order.
  // The whole array is replaced server-side, so the host stays the
  // single source of truth for ordering + dedup. On write failure we
  // re-fetch the authoritative list rather than guess a rollback
  // target — other writes may still be in flight, so the server is the
  // only honest source of "what's actually saved".
  const toggleFavorite = useCallback(
    (track: PluginTrack) => {
      const seq = ++favoriteSeqRef.current;
      const current = favoritesRef.current;
      const next = current.some((f) => f.id === track.id)
        ? current.filter((f) => f.id !== track.id)
        : [
            ...current,
            {
              id: track.id,
              title: track.title,
              artist: track.artist,
              album: track.album,
              artworkUrl: track.artworkUrl,
            },
          ];
      applyFavorites(next);
      const profileAtToggle = profileIdRef.current;
      writeChainRef.current = writeChainRef.current
        // A prior failure must not break the chain for later writes.
        .catch(() => {})
        .then(() => {
          // Profile switched while this write waited its turn: skip it.
          // The backend writes to whatever profile is active NOW, so
          // persisting here would corrupt the new profile with the old
          // one's list. The switched-to profile already reloaded its
          // own favorites; the trade-off is that this last toggle is
          // not saved to the profile it was made in (a rare edge — the
          // write is local-SQLite fast, the switch window tiny).
          if (profileIdRef.current !== profileAtToggle) return;
          return setPluginFavorites(PLUGIN_ID, next);
        })
        .catch(async (e) => {
          // Only the latest toggle may surface an error / overwrite the
          // optimistic state with the server's authoritative list — an
          // earlier write's re-fetch (or error) would otherwise clobber
          // a newer optimistic state a later write already persisted.
          // Re-check after the await too, since a fresh toggle can land
          // mid-fetch.
          if (favoriteSeqRef.current !== seq) return;
          setError(e instanceof Error ? e.message : String(e));
          // Don't re-fetch across a profile switch — that would pull the
          // wrong profile's list into view.
          if (profileIdRef.current !== profileAtToggle) return;
          try {
            const fresh = await getPluginFavorites(PLUGIN_ID);
            if (favoriteSeqRef.current === seq) applyFavorites(fresh);
          } catch {
            /* leave optimistic state; the next load re-syncs */
          }
        });
    },
    [applyFavorites],
  );

  const openFavorites = useCallback(() => {
    // Invalidate any in-flight category/search resolve so its
    // continuation doesn't paint tracks on top of the favorites view.
    resolveReqRef.current += 1;
    setActiveEntry(null);
    setSearchActive(false);
    setFavoritesActive(true);
    setResolving(false);
    setError(null);
    setTracks([]);
  }, []);

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
    setFavoritesActive(false);
    setTracks([]);
    setPlayingId(null);
    setLoadingId(null);
  }, []);

  const playTrack = useCallback(
    async (track: PluginTrack) => {
      // Click on a row that's currently loading → cancel the request
      // and clear the spinner. NO `playerStop` because the audio
      // engine has not been told to play anything yet — firing Stop
      // here would also interrupt whatever local track was playing
      // before the user clicked the radio (regression from PR #228).
      if (loadingId === track.id) {
        streamReqRef.current += 1;
        setLoadingId(null);
        return;
      }
      // Click on a row that is confirmed playing → toggle off via
      // the engine. The `player:state -> idle` listener clears
      // `playingId` for us.
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
      // Show the Loader2 spinner immediately so the click gives
      // tactile feedback while `pluginStreamUrl` + `playerPlayUrl`
      // are in flight (the radio-browser → http_source probe can
      // take several hundred ms). Deliberately NOT touching
      // `playingId` here — painting a Pause icon before the engine
      // actually plays anything is the same trap that broke clicks
      // in PR #228 (users re-click thinking the first didn't
      // register, the second click hits the toggle-off branch,
      // `playerPlayUrl` never fires because its token is stale).
      setLoadingId(track.id);
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
        if (streamReqRef.current !== token) return;
        // Engine accepted the command. Move from loading → playing.
        // The decoder still has to probe + decode before audio is
        // audible, but UX-wise the row should now show Pause so a
        // second click toggles off cleanly.
        setLoadingId(null);
        setPlayingId(track.id);
      } catch (e) {
        if (streamReqRef.current !== token) return;
        setError(e instanceof Error ? e.message : String(e));
        setLoadingId(null);
      }
    },
    [playingId, loadingId],
  );

  const showCategoryList =
    activeEntry === null && !searchActive && !favoritesActive;

  // Favorites render through the exact same row component as resolved
  // tracks. Radio favorites are always live (`durationMs: 0`) and
  // carry no ICY hint — the host re-probes that on play.
  const favoriteTracks = useMemo<PluginTrack[]>(
    () =>
      favorites.map((f) => ({
        id: f.id,
        title: f.title,
        artist: f.artist,
        album: f.album,
        durationMs: 0,
        artworkUrl: f.artworkUrl,
        icyUrl: null,
      })),
    [favorites],
  );
  const displayTracks = favoritesActive ? favoriteTracks : tracks;

  // Two paths converge on "the plugin host won't honour our calls":
  //   1. The user flipped the toggle off in Settings → Plugins
  //      *while* this view was mounted. `pluginAvailable` flips first
  //      because the Settings card dispatches the bus event.
  //   2. The initial `pluginListEntries` call landed before the bus
  //      event fired, returned `plugin disabled`, and parked the
  //      error in `error`. We sniff for that wording too so the
  //      same empty-state covers both races.
  // We still leave generic backend errors visible (network blip,
  // wasm trap) because hiding those behind a "disable me" pitch
  // would mask real bugs.
  const showUnavailableEmptyState =
    !pluginAvailable || isPluginUnavailableError(error);

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

      {/* Generic backend errors only — the plugin-unavailable case is
          rendered as a full empty state below so the user gets a
          single, actionable surface instead of a noisy red banner
          stacked on top of a stale category grid. */}
      {error && !showUnavailableEmptyState && (
        <div
          role="alert"
          className="mx-6 mt-3 px-3 py-2 bg-red-50 dark:bg-red-950/30 text-xs text-red-700 dark:text-red-300 border border-red-200 dark:border-red-900 rounded"
        >
          {error}
        </div>
      )}

      <div className="flex-1 overflow-y-auto px-6 py-4">
        {showUnavailableEmptyState ? (
          <div className="flex flex-col items-center justify-center text-center py-16 px-6">
            <Puzzle
              size={48}
              className="text-zinc-300 dark:text-zinc-700 mb-3"
              aria-hidden="true"
            />
            <h2 className="text-base font-medium text-zinc-700 dark:text-zinc-200">
              {t("webRadio.unavailableTitle")}
            </h2>
            <p className="text-sm text-zinc-500 dark:text-zinc-400 mt-1 max-w-md">
              {t("webRadio.unavailableHint")}
            </p>
          </div>
        ) : loading ? (
          <div className="text-center text-sm text-zinc-500 dark:text-zinc-400 py-12">
            {t("webRadio.loading")}
          </div>
        ) : showCategoryList ? (
          <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-4 gap-3">
            {/* Favorites pseudo-category, pinned first so saved
                stations are one click away (Receiver-style). Always
                shown — an empty list teaches the star affordance via
                its own empty state. */}
            <button
              type="button"
              onClick={openFavorites}
              className="text-left px-4 py-6 rounded-xl bg-amber-50 dark:bg-amber-950/20 hover:bg-amber-100 dark:hover:bg-amber-950/40 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-amber-500"
            >
              <div className="flex items-center gap-2 text-base font-medium text-zinc-900 dark:text-white">
                <Star
                  size={18}
                  className="text-amber-500 shrink-0"
                  fill="currentColor"
                  aria-hidden="true"
                />
                <span>{t("webRadio.favorites")}</span>
                {favorites.length > 0 && (
                  <span className="ml-auto text-xs font-semibold text-amber-600 dark:text-amber-400">
                    {favorites.length}
                  </span>
                )}
              </div>
            </button>
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
        ) : displayTracks.length === 0 ? (
          <div className="text-center text-sm text-zinc-500 dark:text-zinc-400 py-12">
            {favoritesActive
              ? t("webRadio.favoritesEmpty")
              : t("webRadio.emptyResults")}
          </div>
        ) : (
          <ul className="divide-y divide-zinc-200 dark:divide-zinc-800">
            {displayTracks.map((track, idx) => {
              const isPlaying = playingId === track.id;
              const isLoading = loadingId === track.id;
              const favorited = isFavorite(track.id);
              // The plugin encodes `track.id` as `url:<stream>` and
              // radio-browser sometimes returns multiple distinct
              // stations sharing the same stream URL. Without an
              // index prefix React collapses those rows into one
              // (duplicate-key warning) and click handlers can fire
              // against the wrong instance. The index disambiguates
              // for React; `track.id` stays as-is for `playingId`
              // comparison + `pluginStreamUrl` lookup.
              return (
                <li
                  key={`${idx}-${track.id}`}
                  className="py-2 flex items-center gap-1"
                >
                  <button
                    type="button"
                    onClick={() => {
                      void playTrack(track);
                    }}
                    className="flex-1 min-w-0 text-left px-2 py-1.5 rounded hover:bg-zinc-100 dark:hover:bg-zinc-800 flex items-center gap-3 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
                  >
                    <span
                      className={`w-8 h-8 rounded flex items-center justify-center shrink-0 ${
                        isPlaying || isLoading
                          ? "bg-emerald-500 text-white"
                          : "bg-zinc-200 dark:bg-zinc-700 text-zinc-600 dark:text-zinc-300"
                      }`}
                      aria-hidden="true"
                    >
                      {isLoading ? (
                        <Loader2 size={14} className="animate-spin" />
                      ) : isPlaying ? (
                        <Pause size={14} />
                      ) : (
                        <Play size={14} />
                      )}
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
                  <button
                    type="button"
                    onClick={() => toggleFavorite(track)}
                    aria-pressed={favorited}
                    aria-label={
                      favorited
                        ? t("webRadio.removeFavorite")
                        : t("webRadio.addFavorite")
                    }
                    title={
                      favorited
                        ? t("webRadio.removeFavorite")
                        : t("webRadio.addFavorite")
                    }
                    className="shrink-0 w-9 h-9 rounded flex items-center justify-center text-zinc-400 hover:text-amber-500 hover:bg-zinc-100 dark:hover:bg-zinc-800 focus:outline-none focus-visible:ring-2 focus-visible:ring-amber-500 transition-colors"
                  >
                    <Star
                      size={16}
                      className={favorited ? "text-amber-500" : ""}
                      fill={favorited ? "currentColor" : "none"}
                      aria-hidden="true"
                    />
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

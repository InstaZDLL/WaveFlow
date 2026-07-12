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
  Globe,
  MapPin,
  Database,
} from "lucide-react";

import { playerPlayUrl, playerStop } from "../../lib/tauri/player";
import {
  pluginListEntries,
  pluginResolve,
  pluginStreamUrl,
  type PluginEntry,
  type PluginTrack,
} from "../../lib/tauri/plugins";
import { usePluginAvailability } from "../../hooks/usePluginAvailability";
import {
  useWebRadioFavorites,
  WEB_RADIO_PLUGIN_ID,
} from "../../hooks/useWebRadioFavorites";
import { WEB_RADIO_COUNTRY_CODES } from "../../lib/webRadioCountries";
import { getOfflineMode } from "../../lib/tauri/offline";
import {
  radioCatalogueStatus,
  resolveRadioCatalogue,
  getRadioPreferredCountry,
  setRadioPreferredCountry,
} from "../../lib/tauri/webRadioCatalogue";

const PLUGIN_ID = WEB_RADIO_PLUGIN_ID;

/** Localized country name for an ISO 3166-1 alpha-2 code, falling back
 *  to the bare code when `Intl.DisplayNames` is unavailable / throws. */
function countryLabel(uiLang: string, code: string): string {
  try {
    const name = new Intl.DisplayNames([uiLang], { type: "region" }).of(code);
    if (name) return name;
  } catch {
    /* fall through */
  }
  return code;
}

/** IANA time-zone → ISO 3166-1 alpha-2 country. Reflects the user's
 *  physical location far better than the OS UI language (an en-US
 *  Windows install outside the US is extremely common). Not exhaustive
 *  — falls through to the locale region when a zone isn't mapped. */
const TIMEZONE_TO_REGION: Record<string, string> = {
  // Europe
  "Europe/Paris": "FR",
  "Europe/London": "GB",
  "Europe/Dublin": "IE",
  "Europe/Madrid": "ES",
  "Europe/Lisbon": "PT",
  "Europe/Berlin": "DE",
  "Europe/Zurich": "CH",
  "Europe/Vienna": "AT",
  "Europe/Rome": "IT",
  "Europe/Amsterdam": "NL",
  "Europe/Brussels": "BE",
  "Europe/Luxembourg": "LU",
  "Europe/Copenhagen": "DK",
  "Europe/Oslo": "NO",
  "Europe/Stockholm": "SE",
  "Europe/Helsinki": "FI",
  "Europe/Warsaw": "PL",
  "Europe/Prague": "CZ",
  "Europe/Bratislava": "SK",
  "Europe/Budapest": "HU",
  "Europe/Bucharest": "RO",
  "Europe/Sofia": "BG",
  "Europe/Athens": "GR",
  "Europe/Zagreb": "HR",
  "Europe/Belgrade": "RS",
  "Europe/Ljubljana": "SI",
  "Europe/Kyiv": "UA",
  "Europe/Kiev": "UA",
  "Europe/Moscow": "RU",
  "Europe/Istanbul": "TR",
  // Americas
  "America/New_York": "US",
  "America/Chicago": "US",
  "America/Denver": "US",
  "America/Phoenix": "US",
  "America/Los_Angeles": "US",
  "America/Anchorage": "US",
  "Pacific/Honolulu": "US",
  "America/Toronto": "CA",
  "America/Vancouver": "CA",
  "America/Edmonton": "CA",
  "America/Winnipeg": "CA",
  "America/Halifax": "CA",
  "America/Mexico_City": "MX",
  "America/Sao_Paulo": "BR",
  "America/Argentina/Buenos_Aires": "AR",
  "America/Santiago": "CL",
  "America/Bogota": "CO",
  "America/Lima": "PE",
  // Asia / Middle East
  "Asia/Tokyo": "JP",
  "Asia/Seoul": "KR",
  "Asia/Shanghai": "CN",
  "Asia/Hong_Kong": "HK",
  "Asia/Taipei": "TW",
  "Asia/Singapore": "SG",
  "Asia/Bangkok": "TH",
  "Asia/Jakarta": "ID",
  "Asia/Kuala_Lumpur": "MY",
  "Asia/Manila": "PH",
  "Asia/Kolkata": "IN",
  "Asia/Calcutta": "IN",
  "Asia/Dubai": "AE",
  "Asia/Riyadh": "SA",
  "Asia/Jerusalem": "IL",
  "Asia/Tel_Aviv": "IL",
  "Asia/Tehran": "IR",
  // Oceania / Africa
  "Australia/Sydney": "AU",
  "Australia/Melbourne": "AU",
  "Australia/Brisbane": "AU",
  "Australia/Perth": "AU",
  "Pacific/Auckland": "NZ",
  "Africa/Johannesburg": "ZA",
  "Africa/Cairo": "EG",
  "Africa/Lagos": "NG",
  "Africa/Casablanca": "MA",
  "Africa/Tunis": "TN",
  "Africa/Algiers": "DZ",
};

/** Best-effort ISO 3166-1 alpha-2 region for the "Local stations"
 *  default. Prefers the IANA time zone (reflects physical location)
 *  and falls back to the browser locale region ("fr-FR" → "FR") when
 *  the zone isn't mapped. Returns null when neither yields a region
 *  (e.g. a bare "en"). Used as the initial default; overridden by the
 *  user's pinned preference loaded from the backend on mount. */
function detectLocalRegion(): string | null {
  try {
    const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
    const fromTz = tz ? TIMEZONE_TO_REGION[tz] : undefined;
    if (fromTz) return fromTz;
  } catch {
    // fall through to locale
  }
  try {
    const region = new Intl.Locale(navigator.language).region;
    return region && /^[A-Za-z]{2}$/.test(region) ? region.toUpperCase() : null;
  } catch {
    return null;
  }
}

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
  const { t, i18n } = useTranslation();
  const pluginAvailable = usePluginAvailability(PLUGIN_ID);
  const [entries, setEntries] = useState<PluginEntry[]>([]);
  const [activeEntry, setActiveEntry] = useState<PluginEntry | null>(null);
  const [tracks, setTracks] = useState<PluginTrack[]>([]);
  const [loading, setLoading] = useState(true);
  const [resolving, setResolving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [searchTerm, setSearchTerm] = useState("");
  const [searchActive, setSearchActive] = useState(false);
  // Per-profile saved stations (issue #289) come from the shared hook
  // so this view, the PlayerBar and the mini-player favorite-station
  // star never drift. The "Favorites" pseudo-category replays them with
  // zero network (the stream URL rides inside `track.id` as `url:…`).
  const { favorites, isFavorite, toggleFavorite } = useWebRadioFavorites();
  const [favoritesActive, setFavoritesActive] = useState(false);
  // "Local stations" shortcut country. Seeded from the webview locale on
  // first load and overridden by the user's saved preference, so someone
  // on an EN-US Windows who is not in the US only has to pick their country
  // once from the dropdown — the shortcut remembers it from then on.
  const [localRegion, setLocalRegion] = useState<string | null>(
    detectLocalRegion,
  );
  // Guards against the mount effect's async callback overwriting a country
  // the user has already picked before the promise resolved.
  const localRegionSelectedRef = useRef(false);
  useEffect(() => {
    getRadioPreferredCountry()
      .then((code) => {
        if (code && !localRegionSelectedRef.current) setLocalRegion(code);
      })
      .catch(() => {});
  }, []);
  // Offline catalogue routing (#289 #4). `useLocal` flips browse + search
  // from the live plugin to the downloaded local catalogue when offline mode
  // is on, or when the user enabled "local-first" (Settings → Data) AND a
  // catalogue is present. Resolved once on mount — navigating away unmounts
  // the view, so a Settings change is picked up on the next visit.
  const [useLocal, setUseLocal] = useState(false);
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        // Offline mode is evaluated first and wins on its own: when offline we
        // must route to the local catalogue regardless of whether the status
        // call below succeeds (a failed status must not drop us back to the
        // live plugin, which can't reach the network anyway).
        const offline = await getOfflineMode();
        if (cancelled) return;
        if (offline) {
          setUseLocal(true);
          return;
        }
        // Online: prefer the local catalogue only when local-first is on AND a
        // *completed* sync exists (`lastSyncedAt`, not a bare row count — a
        // partial/interrupted import has rows but no marker, and the backend
        // resolve serves it nothing anyway).
        const status = await radioCatalogueStatus();
        if (cancelled) return;
        setUseLocal(status.localFirst && status.lastSyncedAt != null);
      } catch {
        /* status unavailable → stay on the live plugin */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);
  // One resolver for both browse (openEntry) and search: the local catalogue
  // answers the SAME query tokens as the plugin and returns the SAME shape.
  const resolveStations = useCallback(
    (query: string): Promise<PluginTrack[]> =>
      useLocal ? resolveRadioCatalogue(query) : pluginResolve(PLUGIN_ID, query),
    [useLocal],
  );
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

  const openEntry = useCallback(
    async (entry: PluginEntry) => {
      const token = ++resolveReqRef.current;
      setActiveEntry(entry);
      setSearchActive(false);
      setFavoritesActive(false);
      setResolving(true);
      setError(null);
      setTracks([]);
      try {
        const list = await resolveStations(entry.query);
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
    },
    [resolveStations],
  );

  // Country browsing reuses the entry-resolve machinery: a country is
  // just a synthetic entry whose query is the `country:<ISO2>` token
  // the plugin understands. Picking one from the dropdown / the local
  // shortcut funnels through `openEntry`, so token invalidation, the
  // resolving spinner, error handling + the back button all work
  // unchanged.
  const openCountry = useCallback(
    (code: string, name: string) => {
      void openEntry({ label: name, query: `country:${code}`, iconUrl: null });
    },
    [openEntry],
  );

  // Countries localized + sorted in the UI language. Recomputed only
  // when the language changes.
  const countries = useMemo(
    () =>
      WEB_RADIO_COUNTRY_CODES.map((code) => ({
        code,
        name: countryLabel(i18n.language, code),
      })).sort((a, b) => a.name.localeCompare(b.name, i18n.language)),
    [i18n.language],
  );

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
      const list = await resolveStations(term);
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
  }, [searchTerm, resolveStations]);

  // Build the favorite record for a station row so the shared hook's
  // toggle (which takes a `PluginFavorite`) can save it.
  const toggleFavoriteTrack = useCallback(
    (track: PluginTrack) => {
      toggleFavorite({
        id: track.id,
        title: track.title,
        artist: track.artist,
        album: track.album,
        artworkUrl: track.artworkUrl,
      });
    },
    [toggleFavorite],
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
        {/* Local-catalogue indicator: browse + search are answered from the
            downloaded snapshot, not the live radio-browser API. */}
        {useLocal && (
          <span
            className="shrink-0 inline-flex items-center gap-1 px-2 py-1 rounded-full text-[11px] font-medium bg-amber-100 dark:bg-amber-950/40 text-amber-700 dark:text-amber-300"
            title={t("webRadio.localCatalogueHint")}
          >
            <Database size={12} aria-hidden="true" />
            {t("webRadio.localCatalogue")}
          </span>
        )}
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
        {/* Country picker — selecting one funnels through `openCountry`
            → the plugin's `country:<ISO2>` token. Value is pinned to ""
            so it acts as an action trigger and is reusable after a
            pick (it doesn't track the active country). */}
        <div className="relative">
          <Globe
            size={14}
            className="absolute left-2.5 top-1/2 -translate-y-1/2 text-zinc-400 pointer-events-none"
            aria-hidden="true"
          />
          <select
            value=""
            onChange={(e) => {
              const code = e.target.value;
              if (!code) return;
              const picked = countries.find((c) => c.code === code);
              // Persist first — navigate and update the shortcut only after
              // persistence succeeds so a rejected write cannot leave the
              // session showing an unpersisted country.
              void setRadioPreferredCountry(code)
                .then(() => {
                  localRegionSelectedRef.current = true;
                  setLocalRegion(code);
                  openCountry(code, picked?.name ?? code);
                })
                .catch((e: unknown) => {
                  setError(e instanceof Error ? e.message : String(e));
                });
            }}
            aria-label={t("webRadio.browseByCountry")}
            className="pl-8 pr-3 py-1.5 text-sm rounded bg-zinc-100 dark:bg-zinc-800 text-zinc-900 dark:text-white border border-zinc-200 dark:border-zinc-700 focus:outline-none focus:ring-2 focus:ring-emerald-500 max-w-40"
          >
            <option value="">{t("webRadio.browseByCountry")}</option>
            {countries.map((c) => (
              <option key={c.code} value={c.code}>
                {c.name}
              </option>
            ))}
          </select>
        </div>
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
            {/* Local stations — one-click shortcut to the user's own
                country, detected from the webview locale. Hidden when
                the locale carries no region (e.g. a bare "en"); the
                country dropdown still covers every country. */}
            {localRegion && (
              <button
                type="button"
                onClick={() =>
                  openCountry(
                    localRegion,
                    countryLabel(i18n.language, localRegion),
                  )
                }
                className="text-left px-4 py-6 rounded-xl bg-sky-50 dark:bg-sky-950/20 hover:bg-sky-100 dark:hover:bg-sky-950/40 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-sky-500"
              >
                <div className="flex items-center gap-2 text-base font-medium text-zinc-900 dark:text-white">
                  <MapPin
                    size={18}
                    className="text-sky-500 shrink-0"
                    aria-hidden="true"
                  />
                  <span className="min-w-0 truncate">
                    {t("webRadio.localStations")}
                  </span>
                </div>
                <div className="mt-1 text-xs text-zinc-500 dark:text-zinc-400 truncate">
                  {countryLabel(i18n.language, localRegion)}
                </div>
              </button>
            )}
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
                    onClick={() => toggleFavoriteTrack(track)}
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

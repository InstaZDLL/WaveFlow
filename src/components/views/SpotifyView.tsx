import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Headphones, Loader2, Play, Search, ExternalLink } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { ViewId } from "../../types";
import { usePlayer } from "../../hooks/usePlayer";
import { useSpotify } from "../../hooks/useSpotify";
import {
  spotifyListPlaylists,
  spotifySearch,
  type SpotifyAlbumLite,
  type SpotifyArtistLite,
  type SpotifyPlaylistLite,
  type SpotifySearchResults,
  type SpotifyTrackLite,
} from "../../lib/tauri/spotify";

interface SpotifyViewProps {
  onNavigate: (view: ViewId) => void;
}

export function SpotifyView({ onNavigate }: SpotifyViewProps) {
  const { t } = useTranslation();
  const spotify = useSpotify();
  const { loadPlaylistTracks } = spotify;
  const { playSpotifyTrack, playSpotifyContext } = usePlayer();
  const [playlists, setPlaylists] = useState<SpotifyPlaylistLite[]>([]);
  const [selectedPlaylist, setSelectedPlaylist] =
    useState<SpotifyPlaylistLite | null>(null);
  const [playlistTracks, setPlaylistTracks] = useState<SpotifyTrackLite[]>([]);
  const [isLoadingPlaylists, setIsLoadingPlaylists] = useState(false);
  const [isLoadingTracks, setIsLoadingTracks] = useState(false);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SpotifySearchResults>({
    tracks: [],
    albums: [],
    artists: [],
  });
  const [error, setError] = useState<string | null>(null);

  const canUseSpotify = !!(
    spotify.status?.configured && spotify.status.connected
  );

  const refreshPlaylists = useCallback(async () => {
    if (!canUseSpotify) return;
    setIsLoadingPlaylists(true);
    setError(null);
    try {
      const list = await spotifyListPlaylists();
      setPlaylists(list);
      setSelectedPlaylist((current) => current ?? list[0] ?? null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setIsLoadingPlaylists(false);
    }
  }, [canUseSpotify]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    refreshPlaylists();
  }, [refreshPlaylists]);

  useEffect(() => {
    if (!selectedPlaylist) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setPlaylistTracks([]);
      return;
    }
    let cancelled = false;
    setIsLoadingTracks(true);
    loadPlaylistTracks(selectedPlaylist)
      .then((tracks) => {
        if (!cancelled) setPlaylistTracks(tracks);
      })
      .catch((err) => {
        if (!cancelled)
          setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        if (!cancelled) setIsLoadingTracks(false);
      });
    return () => {
      cancelled = true;
    };
  }, [selectedPlaylist, loadPlaylistTracks]);

  useEffect(() => {
    const trimmed = query.trim();
    if (!trimmed) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setResults({ tracks: [], albums: [], artists: [] });
      return;
    }
    const id = window.setTimeout(() => {
      spotifySearch(trimmed)
        .then(setResults)
        .catch((err) =>
          setError(err instanceof Error ? err.message : String(err)),
        );
    }, 350);
    return () => window.clearTimeout(id);
  }, [query]);

  const hasResults = useMemo(
    () =>
      results.tracks.length > 0 ||
      results.albums.length > 0 ||
      results.artists.length > 0,
    [results],
  );

  if (!spotify.status?.configured) {
    return (
      <EmptySpotifyState
        title={t("spotify.notConfiguredTitle", "Register your Spotify app")}
        message={t(
          "spotify.notConfiguredMessage",
          "Spotify requires every third-party app to register a free Client ID before any playback. Create one on the Spotify Developer dashboard, then paste it in WaveFlow's Settings -> Integrations.",
        )}
        actions={[
          {
            label: t(
              "spotify.openDashboard",
              "Open Spotify Developer dashboard",
            ),
            external: true,
            onAction: () =>
              openUrl("https://developer.spotify.com/dashboard").catch((err) =>
                console.error("[SpotifyView] open dashboard failed", err),
              ),
            primary: true,
          },
          {
            label: t("spotify.openSettings", "Open Settings"),
            onAction: () => onNavigate("settings"),
          },
        ]}
      />
    );
  }

  if (!spotify.status.connected) {
    return (
      <EmptySpotifyState
        title={t("spotify.notConnectedTitle", "Spotify is not connected")}
        message={t(
          "spotify.notConnectedMessage",
          "Your Client ID is set. Sign in to your Spotify Premium account from Settings to load your playlists and play music.",
        )}
        actions={[
          {
            label: t("spotify.openSettings", "Open Settings"),
            onAction: () => onNavigate("settings"),
            primary: true,
          },
        ]}
      />
    );
  }

  return (
    <div className="h-full overflow-y-auto px-8 py-6">
      <div className="flex items-center justify-between mb-6">
        <div>
          <h1 className="text-2xl font-bold text-zinc-900 dark:text-white">
            Spotify
          </h1>
          <p className="text-sm text-zinc-500">
            {spotify.deviceId
              ? t("spotify.deviceReady", "WaveFlow Spotify device ready")
              : t("spotify.devicePending", "Preparing Spotify playback...")}
          </p>
        </div>
        {isLoadingPlaylists && (
          <Loader2 size={20} className="animate-spin text-zinc-400" />
        )}
      </div>

      {(error || spotify.error) && (
        <div className="mb-4 rounded-lg border border-rose-200 bg-rose-50 px-4 py-3 text-sm text-rose-700 dark:border-rose-900/60 dark:bg-rose-950/30 dark:text-rose-300">
          {error ?? spotify.error}
        </div>
      )}

      <div className="relative mb-6 max-w-xl">
        <Search
          size={18}
          className="absolute left-3 top-1/2 -translate-y-1/2 text-zinc-400"
        />
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t("spotify.searchPlaceholder", "Search Spotify")}
          className="w-full rounded-lg border border-zinc-200 bg-white py-2 pl-10 pr-3 text-sm text-zinc-900 outline-none focus:border-emerald-500 dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-100"
        />
      </div>

      {hasResults ? (
        <div className="space-y-8">
          <TrackSection
            title={t("spotify.tracks", "Tracks")}
            tracks={results.tracks}
            onPlay={playSpotifyTrack}
          />
          <AlbumSection
            albums={results.albums}
            onPlay={(album) => playSpotifyContext(album.uri)}
          />
          <ArtistSection artists={results.artists} />
        </div>
      ) : (
        <div className="grid grid-cols-[280px_minmax(0,1fr)] gap-6">
          <div className="space-y-2">
            <h2 className="text-xs font-bold uppercase tracking-widest text-zinc-400">
              {t("spotify.playlists", "Playlists")}
            </h2>
            <div className="space-y-1">
              {playlists.map((playlist) => (
                <button
                  key={playlist.id}
                  type="button"
                  onClick={() => setSelectedPlaylist(playlist)}
                  className={`w-full rounded-lg px-3 py-2 text-left transition-colors ${
                    selectedPlaylist?.id === playlist.id
                      ? "bg-emerald-50 text-emerald-700 dark:bg-emerald-900/20 dark:text-emerald-300"
                      : "text-zinc-700 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
                  }`}
                >
                  <div className="truncate text-sm font-medium">
                    {playlist.name}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {playlist.track_count} tracks
                  </div>
                </button>
              ))}
            </div>
          </div>

          <div>
            {selectedPlaylist && (
              <div className="mb-4 flex items-center gap-4">
                <RemoteCover
                  url={selectedPlaylist.image_url}
                  alt={selectedPlaylist.name}
                  className="h-20 w-20"
                />
                <div className="min-w-0">
                  <h2 className="truncate text-xl font-semibold text-zinc-900 dark:text-white">
                    {selectedPlaylist.name}
                  </h2>
                  <p className="text-sm text-zinc-500">
                    {selectedPlaylist.owner_name}
                  </p>
                </div>
                <button
                  type="button"
                  onClick={() => playSpotifyContext(selectedPlaylist.uri)}
                  disabled={!spotify.deviceId}
                  className="ml-auto flex h-10 w-10 items-center justify-center rounded-full bg-emerald-500 text-white hover:bg-emerald-600 disabled:opacity-50"
                >
                  <Play size={18} className="fill-current translate-x-px" />
                </button>
              </div>
            )}
            {isLoadingTracks ? (
              <Loader2 size={22} className="animate-spin text-zinc-400" />
            ) : playlistTracks.length === 0 ? (
              <div className="rounded-lg border border-zinc-200 bg-white px-4 py-8 text-center text-sm text-zinc-500 dark:border-zinc-800 dark:bg-zinc-900">
                {t(
                  "spotify.emptyPlaylist",
                  "No tracks available in this playlist (Spotify may not expose them, local files or unavailable items get filtered out).",
                )}
              </div>
            ) : (
              <TrackSection
                title=""
                tracks={playlistTracks}
                onPlay={playSpotifyTrack}
              />
            )}
          </div>
        </div>
      )}
    </div>
  );
}

interface EmptyStateAction {
  label: string;
  onAction: () => void;
  /** Visual primacy — emerald background vs neutral border. Defaults
   *  to false. Use exactly one primary action per state. */
  primary?: boolean;
  /** Render the external-link glyph alongside the label. Used for
   *  actions that open the system browser. */
  external?: boolean;
}

function EmptySpotifyState({
  title,
  message,
  actions,
}: {
  title: string;
  message: string;
  actions: EmptyStateAction[];
}) {
  return (
    <div className="flex h-full items-center justify-center p-8">
      <div className="max-w-md text-center">
        <div className="mx-auto mb-4 flex h-14 w-14 items-center justify-center rounded-xl bg-emerald-50 text-emerald-500 dark:bg-emerald-900/20">
          <Headphones size={26} />
        </div>
        <h1 className="mb-2 text-xl font-semibold text-zinc-900 dark:text-white">
          {title}
        </h1>
        <p className="mb-5 text-sm text-zinc-500">{message}</p>
        <div className="flex flex-col items-center gap-2">
          {actions.map((action, i) => (
            <button
              key={i}
              type="button"
              onClick={action.onAction}
              className={
                action.primary
                  ? "inline-flex items-center gap-2 rounded-lg bg-emerald-500 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-600"
                  : "inline-flex items-center gap-2 rounded-lg border border-zinc-200 bg-white px-4 py-2 text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700"
              }
            >
              <span>{action.label}</span>
              {action.external && <ExternalLink size={14} aria-hidden="true" />}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}

function TrackSection({
  title,
  tracks,
  onPlay,
}: {
  title: string;
  tracks: SpotifyTrackLite[];
  onPlay: (track: SpotifyTrackLite) => Promise<void>;
}) {
  if (tracks.length === 0) return null;
  return (
    <section>
      {title && (
        <h2 className="mb-3 text-xs font-bold uppercase tracking-widest text-zinc-400">
          {title}
        </h2>
      )}
      <div className="divide-y divide-zinc-100 rounded-lg border border-zinc-200 bg-white dark:divide-zinc-800 dark:border-zinc-800 dark:bg-zinc-900">
        {tracks.map((track) => (
          <button
            key={track.uri}
            type="button"
            onClick={() => onPlay(track)}
            className="flex w-full items-center gap-3 px-3 py-2 text-left hover:bg-zinc-50 dark:hover:bg-zinc-800"
          >
            <RemoteCover
              url={track.image_url}
              alt={track.name}
              className="h-10 w-10"
            />
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm font-medium text-zinc-900 dark:text-zinc-100">
                {track.name}
              </div>
              <div className="truncate text-xs text-zinc-500">
                {track.artist_name}
              </div>
            </div>
            <Play size={16} className="text-zinc-400" />
          </button>
        ))}
      </div>
    </section>
  );
}

function AlbumSection({
  albums,
  onPlay,
}: {
  albums: SpotifyAlbumLite[];
  onPlay: (album: SpotifyAlbumLite) => Promise<void>;
}) {
  if (albums.length === 0) return null;
  return (
    <section>
      <h2 className="mb-3 text-xs font-bold uppercase tracking-widest text-zinc-400">
        Albums
      </h2>
      <div className="grid grid-cols-6 gap-4">
        {albums.map((album) => (
          <button
            key={album.id}
            type="button"
            onClick={() => onPlay(album)}
            className="min-w-0 text-left"
          >
            <RemoteCover
              url={album.image_url}
              alt={album.name}
              className="mb-2 aspect-square w-full"
            />
            <div className="truncate text-sm font-medium text-zinc-900 dark:text-zinc-100">
              {album.name}
            </div>
            <div className="truncate text-xs text-zinc-500">
              {album.artist_name}
            </div>
          </button>
        ))}
      </div>
    </section>
  );
}

function ArtistSection({ artists }: { artists: SpotifyArtistLite[] }) {
  if (artists.length === 0) return null;
  return (
    <section>
      <h2 className="mb-3 text-xs font-bold uppercase tracking-widest text-zinc-400">
        Artists
      </h2>
      <div className="grid grid-cols-6 gap-4">
        {artists.map((artist) => (
          <div key={artist.id} className="min-w-0">
            <RemoteCover
              url={artist.image_url}
              alt={artist.name}
              className="mb-2 aspect-square w-full rounded-full"
            />
            <div className="truncate text-center text-sm font-medium text-zinc-900 dark:text-zinc-100">
              {artist.name}
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}

function RemoteCover({
  url,
  alt,
  className,
}: {
  url: string | null;
  alt: string;
  className: string;
}) {
  return url ? (
    <img
      src={url}
      alt={alt}
      className={`shrink-0 rounded-lg object-cover ${className}`}
    />
  ) : (
    <div
      className={`shrink-0 rounded-lg bg-zinc-100 dark:bg-zinc-800 ${className}`}
    />
  );
}

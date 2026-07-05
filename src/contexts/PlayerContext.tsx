import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  PlayerContext,
  type PlaybackState,
  type RepeatMode,
} from "../hooks/usePlayer";
import { useProfile } from "../hooks/useProfile";
import { useSpotify } from "../hooks/useSpotify";
import type { Track } from "../lib/tauri/track";
import type { SpotifyTrackLite } from "../lib/tauri/spotify";
import {
  playerCycleRepeat,
  playerGetSpeed,
  playerGetState,
  playerListOutputDevices,
  playerNext,
  playerPause,
  playerPlayTracks,
  playerPrevious,
  playerResume,
  playerResumeLast,
  playerSeek,
  playerSetSpeed,
  playerSetVolume,
  playerToggleShuffle,
  getCurrentRadioMetadata,
  fetchRadioArtwork,
  type OutputDevice,
  type PlayerErrorPayload,
  type PlayerPositionPayload,
  type PlayerStatePayload,
  type QueueSource,
  type QueueTrackPayload,
  type RadioMetadata,
} from "../lib/tauri/player";
import type { PluginFavorite } from "../lib/tauri/plugins";
import { enrichArtistDeezer } from "../lib/tauri/detail";
import { isRadioTrack } from "../lib/playerSources";

/**
 * Minimal conversion from the thin `QueueTrackPayload` returned by
 * `player_get_state` to the full `Track` shape the rest of the UI
 * consumes. Fields we don't carry (bitrate, sample rate, file size,
 * year, …) are nulled out — they're not needed for the PlayerBar.
 */
function queuePayloadToTrack(payload: QueueTrackPayload): Track {
  return {
    id: payload.id,
    library_id: 0,
    title: payload.title,
    album_id: null,
    album_title: payload.album_title,
    artist_id: payload.artist_id,
    artist_name: payload.artist_name,
    artist_ids: payload.artist_ids,
    duration_ms: payload.duration_ms,
    track_number: null,
    disc_number: null,
    year: null,
    bitrate: payload.bitrate,
    sample_rate: payload.sample_rate,
    channels: payload.channels,
    bit_depth: payload.bit_depth,
    codec: payload.codec,
    musical_key: null,
    file_path: payload.file_path,
    file_size: payload.file_size,
    added_at: 0,
    artwork_path: payload.artwork_path,
    artwork_path_1x: payload.artwork_path_1x,
    artwork_path_2x: payload.artwork_path_2x,
    rating: null,
  };
}

/**
 * Build the stable station identity (favorite shape, id `url:<stream>`)
 * from a radio-metadata snapshot. `null` when the payload carries no
 * `station_url` (shouldn't happen for a live stream, but keeps the
 * favorite star honest). The station fields are deliberately separate
 * from the now-playing song so the star saves the station, not the
 * current track.
 */
function radioStationFromMetadata(m: RadioMetadata): PluginFavorite | null {
  if (!m.station_url) return null;
  return {
    id: `url:${m.station_url}`,
    title: m.station_name ?? "Live Radio",
    // `PluginFavorite.artist` is non-null (radio-browser always ships a
    // country / "Internet Radio"); fall back to empty when absent.
    artist: m.station_artist ?? "",
    album: null,
    artworkUrl: m.station_artwork,
  };
}

function radioMetadataToTrack(payload: RadioMetadata): Track {
  return {
    id: payload.track_id,
    library_id: 0,
    title: payload.title ?? "Live Radio",
    album_id: null,
    album_title: null,
    artist_id: null,
    artist_name: payload.artist,
    artist_ids: null,
    // 0 = open-ended scrubber. The PlayerBar's progress fields special-
    // case duration_ms === 0 already (live mode for DLNA / Spotify),
    // so the radio inherits that path without extra logic.
    duration_ms: 0,
    track_number: null,
    disc_number: null,
    year: null,
    bitrate: null,
    sample_rate: null,
    channels: null,
    bit_depth: null,
    codec: "Web Radio",
    musical_key: null,
    file_path: "",
    file_size: 0,
    added_at: 0,
    artwork_path: payload.artwork_url,
    artwork_path_1x: null,
    artwork_path_2x: null,
    rating: null,
  };
}

function spotifyTrackToTrack(track: SpotifyTrackLite): Track {
  return {
    id: -1,
    library_id: 0,
    title: track.name,
    album_id: null,
    album_title: track.album_name,
    artist_id: null,
    artist_name: track.artist_name,
    artist_ids: null,
    duration_ms: track.duration_ms,
    track_number: null,
    disc_number: null,
    year: null,
    bitrate: null,
    sample_rate: null,
    channels: null,
    bit_depth: null,
    codec: "Spotify",
    musical_key: null,
    file_path: track.uri,
    file_size: 0,
    added_at: 0,
    artwork_path: track.image_url,
    artwork_path_1x: null,
    artwork_path_2x: null,
    rating: null,
  };
}

/**
 * Provides the audio player state + actions to the whole React tree.
 *
 * The provider:
 * 1. fetches an initial snapshot from the backend at mount,
 * 2. listens for four Tauri events (`player:position`, `player:state`,
 *    `player:track-ended`, `player:error`) and reflects them in local
 *    React state,
 * 3. wraps `invoke()` calls behind convenient actions,
 * 4. debounces volume slider changes into the backend at 60 ms so a
 *    fast drag doesn't flood the command channel.
 */
export function PlayerProvider({ children }: { children: ReactNode }) {
  const { activeProfile } = useProfile();
  const spotify = useSpotify();

  // Queue, NowPlaying, and Lyrics share the same right-edge slot (w-80),
  // so we model "which is open" as a single value rather than three
  // booleans — that guarantees mutual exclusion by construction.
  const [activeRightPanel, setActiveRightPanel] = useState<
    "queue" | "nowPlaying" | "lyrics" | null
  >(null);
  const isQueueOpen = activeRightPanel === "queue";
  const isNowPlayingOpen = activeRightPanel === "nowPlaying";
  const isLyricsOpen = activeRightPanel === "lyrics";
  const [isDeviceMenuOpen, setIsDeviceMenuOpen] = useState(false);

  // Immersive view (issue #328). The now-playing + lyrics overlays
  // merged into a single two-column view, so this is one boolean
  // instead of the old mutually-exclusive enum. `immersiveInitialTab`
  // remembers which entry point opened it — used only by the narrow-
  // window single-column fallback to pick which column shows first;
  // at desktop widths both columns render side by side.
  const [immersiveOpen, setImmersiveOpen] = useState(false);
  const [immersiveInitialTab, setImmersiveInitialTab] = useState<
    "nowPlaying" | "lyrics"
  >("nowPlaying");

  // Cached output device list. Populated at boot + after every device
  // switch so the menu opens instantly with the up-to-date list — the
  // alternative (fetch-on-open) makes the first click feel laggy
  // even with the fast ALSA-hint enumeration we now use on Linux.
  const [outputDevices, setOutputDevices] = useState<OutputDevice[]>([]);

  // Backend-synced state
  const [activeProvider, setActiveProvider] = useState<"local" | "spotify">(
    "local",
  );
  const [playbackState, setPlaybackState] = useState<PlaybackState>("idle");
  const [currentTrack, setCurrentTrack] = useState<Track | null>(null);
  // Stable Web Radio station identity, separate from the now-playing
  // song (which the ICY de-interleaver overwrites). Set from
  // `player:radio-metadata`, hydrated on mount, cleared when a library
  // track plays or playback goes idle.
  const [currentRadioStation, setCurrentRadioStation] =
    useState<PluginFavorite | null>(null);
  const [positionMs, setPositionMs] = useState(0);
  const [durationMs, setDurationMs] = useState(0);

  // Volume: local + debounced backend push
  const [volume, setVolumeState] = useState(80);
  const previousVolumeRef = useRef(80);
  const volumeDebounceRef = useRef<number | null>(null);

  // Shuffle / repeat — local for checkpoint 11, backend-wired in CP12
  const [isShuffled, setIsShuffled] = useState(false);
  const [repeatMode, setRepeatMode] = useState<RepeatMode>("off");

  // Playback speed (0.5×–2×). Pushed to backend immediately — no
  // debounce because the user picks discrete values from a menu and
  // each rebuild costs ~one rubato chunk of audio (negligible).
  const [playbackSpeed, setPlaybackSpeedState] = useState(1.0);

  // Live output-device sample rate (Hz) and channel count, as
  // reported by the engine snapshot. Used by the AudioQualityFooter
  // resampling arrow and by other UI bits that want to surface the
  // bit-perfect vs. resampled distinction. Refreshed on profile load
  // and on every `player:track-changed` because WASAPI exclusive
  // mode can re-open the device at the new track's native rate.
  const [deviceSampleRate, setDeviceSampleRate] = useState<number | null>(null);
  const [deviceChannels, setDeviceChannels] = useState<number | null>(null);
  // Monotonic token for in-flight `playerGetState` refreshes triggered
  // by `player:track-changed`. If the user fires three skips in a row,
  // three snapshot fetches race; without the token an older snapshot
  // resolving second would overwrite the newer one and stick a stale
  // device rate into the UI until the next track change.
  const deviceRefreshTokenRef = useRef(0);

  // Suppress incoming position events while the user drags the
  // progress bar, so the thumb doesn't fight the mouse.
  const isSeekingRef = useRef(false);

  // Token guarding the async Deezer artwork fetch for the now-playing
  // radio song. Each new ICY title (or a fresh hydration) bumps it so a
  // slow fetch that resolves after the song already changed is dropped.
  const radioArtworkTokenRef = useRef(0);

  // Resolve album art for a now-playing radio song (ICY gives only the
  // "Artist - Title" text) and swap it into `currentTrack` once it
  // lands. The station favicon shows until the cover resolves; this is a
  // no-op when title/artist are missing or the fetch fails / returns
  // nothing. The `isRadioTrack` guard means a library track that started
  // playing in the meantime is never clobbered with a stale radio cover,
  // and the token guard drops a fetch superseded by a newer ICY title.
  const fetchRadioArtworkInto = useCallback(
    (title: string | null, artist: string | null) => {
      if (!title || !artist) return;
      const token = ++radioArtworkTokenRef.current;
      void (async () => {
        try {
          const url = await fetchRadioArtwork(artist, title);
          if (!url || token !== radioArtworkTokenRef.current) return;
          setCurrentTrack((prev) =>
            prev && isRadioTrack(prev)
              ? {
                  ...prev,
                  artwork_path: url,
                  artwork_path_1x: null,
                  artwork_path_2x: null,
                }
              : prev,
          );
        } catch (err) {
          console.error("[PlayerContext] fetch radio artwork failed", err);
        }
      })();
    },
    [],
  );

  const effectivePlaybackState =
    activeProvider === "spotify" ? spotify.playbackState : playbackState;
  const isPlaying = effectivePlaybackState === "playing";

  // --- initial snapshot (re-runs on profile switch) ---
  useEffect(() => {
    // Reset all playback state immediately so stale data from the
    // previous profile doesn't linger during the async fetch.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setPlaybackState("idle");
    setCurrentTrack(null);
    setCurrentRadioStation(null);
    setPositionMs(0);
    setDurationMs(0);
    // Close the immersive view on profile switch — otherwise `immersiveOpen`
    // would linger true through the `currentTrack` null window and reopen
    // the view once the new profile's first track lands.
    setImmersiveOpen(false);

    let cancelled = false;
    (async () => {
      try {
        const snap = await playerGetState();
        if (cancelled) return;
        setPlaybackState(snap.state);
        setPositionMs(snap.position_ms);
        // Volume arrives as 0..1 from the snapshot; UI uses 0..100.
        setVolumeState(Math.round(snap.volume * 100));
        previousVolumeRef.current = Math.round(snap.volume * 100);
        setIsShuffled(snap.shuffle);
        setRepeatMode(snap.repeat_mode);
        // Device-side audio fields are unset (0) before the first
        // stream opens; null'ing them out keeps the type honest so
        // downstream UI can skip the "0 kHz" display.
        setDeviceSampleRate(snap.sample_rate > 0 ? snap.sample_rate : null);
        setDeviceChannels(snap.channels > 0 ? snap.channels : null);
        if (snap.current_track) {
          setCurrentTrack(queuePayloadToTrack(snap.current_track));
          setDurationMs(snap.current_track.duration_ms);
        } else if (snap.state !== "idle") {
          // No library row but something's playing → a Web Radio
          // session. `player_get_state` can't carry it, so hydrate from
          // the dedicated snapshot. Critical for the mini-player webview
          // mounting mid-stream (it would otherwise show nothing until
          // the next ICY title change, minutes away).
          try {
            const radio = await getCurrentRadioMetadata();
            if (!cancelled && radio) {
              setCurrentTrack(radioMetadataToTrack(radio));
              setCurrentRadioStation(radioStationFromMetadata(radio));
              setDurationMs(0);
              // Upgrade the station favicon to the song's album cover.
              fetchRadioArtworkInto(radio.title, radio.artist);
            }
          } catch (err) {
            console.error("[PlayerContext] hydrate radio failed", err);
          }
        }
        // Hydrate playback speed in parallel — separate command
        // because it's not part of the main snapshot (the field
        // would force the audio settings cards to know about it
        // too, and they don't).
        try {
          const speed = await playerGetSpeed();
          if (!cancelled) setPlaybackSpeedState(speed);
        } catch (err) {
          console.error("[PlayerContext] hydrate playback speed failed", err);
        }
      } catch (err) {
        console.error("[PlayerContext] initial snapshot failed", err);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [activeProfile, fetchRadioArtworkInto]);

  // --- Tauri event listeners ---
  useEffect(() => {
    const unlisten: UnlistenFn[] = [];
    let cancelled = false;

    (async () => {
      try {
        unlisten.push(
          await listen<PlayerPositionPayload>("player:position", (e) => {
            if (!isSeekingRef.current) {
              setPositionMs(e.payload.ms);
            }
          }),
        );
        unlisten.push(
          await listen<PlayerStatePayload>("player:state", (e) => {
            setPlaybackState(e.payload.state);
            if (e.payload.state === "ended") {
              // Keep currentTrack in view so the PlayerBar still
              // shows metadata until auto-advance swaps it.
            }
            if (e.payload.state === "idle") {
              // Playback stopped — drop the radio station so the
              // PlayerBar star doesn't linger on a dead session.
              setCurrentRadioStation(null);
            }
          }),
        );
        unlisten.push(
          await listen<QueueTrackPayload>("player:track-changed", (e) => {
            setActiveProvider("local");
            // Backend just selected a new track (via play_tracks,
            // next, previous, resume_last, or the analytics task's
            // auto-advance). Reflect it in the PlayerBar
            // immediately — we can't wait for the first position
            // event because it carries no metadata.
            setCurrentTrack(queuePayloadToTrack(e.payload));
            // A library track is now playing → no longer a radio
            // session, so the favorite-station star disappears.
            setCurrentRadioStation(null);
            setDurationMs(e.payload.duration_ms);
            setPositionMs(0);
            // Refresh the device-side fields from the engine: WASAPI
            // exclusive mode may have reopened the stream at the new
            // track's native rate, and we want the AudioQualityFooter
            // resampling arrow to reflect that without polling. Token-
            // guarded so a slow earlier snapshot can't overwrite a
            // faster later one when the user rage-clicks `next`.
            const reqToken = ++deviceRefreshTokenRef.current;
            void (async () => {
              try {
                const snap = await playerGetState();
                if (reqToken !== deviceRefreshTokenRef.current) return;
                setDeviceSampleRate(
                  snap.sample_rate > 0 ? snap.sample_rate : null,
                );
                setDeviceChannels(snap.channels > 0 ? snap.channels : null);
              } catch (err) {
                console.error(
                  "[PlayerContext] refresh device rate failed",
                  err,
                );
              }
            })();
          }),
        );
        unlisten.push(
          await listen<RadioMetadata>("player:radio-metadata", (e) => {
            // Web Radio (LoadUrlAndPlay) emits this in lieu of
            // `player:track-changed` — no library row to look up,
            // metadata rides on the event payload directly. The
            // now-playing song drives `currentTrack`; the stable
            // station identity drives the favorite-station star.
            setActiveProvider("local");
            setCurrentTrack(radioMetadataToTrack(e.payload));
            setCurrentRadioStation(radioStationFromMetadata(e.payload));
            setDurationMs(0);
            setPositionMs(0);
            // The payload only carries the station favicon; fetch the
            // song's real album cover from Deezer and swap it in async.
            fetchRadioArtworkInto(e.payload.title, e.payload.artist);
          }),
        );
        unlisten.push(
          await listen<PlayerErrorPayload>("player:error", (e) => {
            console.error("[player:error]", e.payload.message);
          }),
        );
      } catch (err) {
        console.error("[PlayerContext] listen setup failed", err);
      }
      if (cancelled) unlisten.forEach((u) => u());
    })();

    return () => {
      cancelled = true;
      unlisten.forEach((u) => u());
    };
  }, [fetchRadioArtworkInto]);

  // --- Volume debounce ---
  const setVolume = useCallback(
    (value: number) => {
      const clamped = Math.max(0, Math.min(100, Math.round(value)));
      setVolumeState(clamped);
      if (clamped > 0) previousVolumeRef.current = clamped;

      if (volumeDebounceRef.current != null) {
        window.clearTimeout(volumeDebounceRef.current);
      }
      volumeDebounceRef.current = window.setTimeout(() => {
        if (activeProvider === "spotify") {
          spotify
            .setVolume(clamped)
            .catch((err) =>
              console.error("[PlayerContext] set spotify volume failed", err),
            );
        } else {
          playerSetVolume(clamped / 100).catch((err) =>
            console.error("[PlayerContext] set volume failed", err),
          );
        }
        volumeDebounceRef.current = null;
      }, 60);
    },
    [activeProvider, spotify],
  );

  const setPlaybackSpeed = useCallback((value: number) => {
    const clamped = Math.max(0.5, Math.min(2.0, value));
    setPlaybackSpeedState(clamped);
    playerSetSpeed(clamped).catch((err) =>
      console.error("[PlayerContext] set speed failed", err),
    );
  }, []);

  const toggleMute = useCallback(() => {
    setVolumeState((current) => {
      const next = current > 0 ? 0 : previousVolumeRef.current || 50;
      if (current > 0) previousVolumeRef.current = current;
      // Mute is immediate — no debounce.
      if (activeProvider === "spotify") {
        spotify
          .setVolume(next)
          .catch((err) =>
            console.error("[PlayerContext] toggle spotify mute failed", err),
          );
      } else {
        playerSetVolume(next / 100).catch((err) =>
          console.error("[PlayerContext] toggle mute failed", err),
        );
      }
      return next;
    });
  }, [activeProvider, spotify]);

  // --- Tear down pending debounce on unmount ---
  useEffect(() => {
    return () => {
      if (volumeDebounceRef.current != null) {
        window.clearTimeout(volumeDebounceRef.current);
      }
    };
  }, []);

  // --- Backend actions ---
  const playTracks = useCallback(
    async (
      tracks: Track[],
      startIndex: number,
      source: { type: QueueSource; id: number | null },
    ) => {
      if (
        tracks.length === 0 ||
        startIndex < 0 ||
        startIndex >= tracks.length
      ) {
        return;
      }
      const chosen = tracks[startIndex];
      if (activeProvider === "spotify" && spotify.playbackState === "playing") {
        await spotify
          .togglePlayback()
          .catch((err) =>
            console.error("[PlayerContext] pause spotify failed", err),
          );
      }
      setActiveProvider("local");
      // Optimistic UI: show the clicked track + duration immediately
      // so the PlayerBar doesn't lag the invoke round-trip.
      setCurrentTrack(chosen);
      setDurationMs(chosen.duration_ms);
      setPositionMs(0);
      setPlaybackState("loading");

      try {
        await playerPlayTracks(
          source.type,
          source.id,
          tracks.map((t) => t.id),
          startIndex,
        );
      } catch (err) {
        console.error("[PlayerContext] play tracks failed", err);
        setPlaybackState("idle");
      }
    },
    [activeProvider, spotify],
  );

  const playSpotifyTrack = useCallback(
    async (track: SpotifyTrackLite) => {
      setActiveProvider("spotify");
      setCurrentTrack(spotifyTrackToTrack(track));
      setDurationMs(track.duration_ms);
      setPositionMs(0);
      setPlaybackState("loading");
      try {
        await spotify.playTrack(track);
      } catch (err) {
        console.error("[PlayerContext] play spotify track failed", err);
        setPlaybackState("idle");
      }
    },
    [spotify],
  );

  const playSpotifyContext = useCallback(
    async (contextUri: string) => {
      setActiveProvider("spotify");
      setPlaybackState("loading");
      try {
        await spotify.playContext(contextUri);
      } catch (err) {
        console.error("[PlayerContext] play spotify context failed", err);
        setPlaybackState("idle");
      }
    },
    [spotify],
  );

  const togglePlayback = useCallback(async () => {
    try {
      if (activeProvider === "spotify") {
        await spotify.togglePlayback();
        return;
      }
      if (playbackState === "playing") {
        await playerPause();
      } else if (playbackState === "paused") {
        await playerResume();
      } else if (currentTrack != null) {
        // Idle / ended with a restored current track: load it at the
        // persisted position and start playing.
        setPlaybackState("loading");
        await playerResumeLast();
      }
    } catch (err) {
      console.error("[PlayerContext] toggle playback failed", err);
      setPlaybackState("idle");
    }
  }, [activeProvider, spotify, playbackState, currentTrack]);

  const next = useCallback(async () => {
    try {
      if (activeProvider === "spotify") {
        await spotify.next();
        return;
      }
      await playerNext();
    } catch (err) {
      console.error("[PlayerContext] next failed", err);
    }
  }, [activeProvider, spotify]);

  const previous = useCallback(async () => {
    try {
      if (activeProvider === "spotify") {
        await spotify.previous();
        return;
      }
      await playerPrevious();
    } catch (err) {
      console.error("[PlayerContext] previous failed", err);
    }
  }, [activeProvider, spotify]);

  const seek = useCallback(
    async (ms: number) => {
      // Optimistic: update the UI position immediately; the backend
      // will also emit player:position after the seek lands.
      setPositionMs(ms);
      try {
        if (activeProvider === "spotify") {
          await spotify.seek(ms);
          return;
        }
        await playerSeek(ms);
      } catch (err) {
        console.error("[PlayerContext] seek failed", err);
      }
    },
    [activeProvider, spotify],
  );

  const setSeeking = useCallback((value: boolean) => {
    isSeekingRef.current = value;
  }, []);

  // --- Shuffle / repeat (backend-wired) ---
  const toggleShuffle = useCallback(async () => {
    // Optimistic UI flip; on backend error we rollback.
    setIsShuffled((prev) => !prev);
    try {
      const next = await playerToggleShuffle();
      setIsShuffled(next);
    } catch (err) {
      console.error("[PlayerContext] toggle shuffle failed", err);
      setIsShuffled((prev) => !prev);
    }
  }, []);

  const cycleRepeatMode = useCallback(async () => {
    const nextMode: RepeatMode =
      repeatMode === "off" ? "all" : repeatMode === "all" ? "one" : "off";
    setRepeatMode(nextMode);
    try {
      const confirmed = await playerCycleRepeat();
      setRepeatMode(confirmed);
    } catch (err) {
      console.error("[PlayerContext] cycle repeat failed", err);
      setRepeatMode(repeatMode); // rollback
    }
  }, [repeatMode]);

  const toggleQueue = useCallback(() => {
    setActiveRightPanel((p) => (p === "queue" ? null : "queue"));
  }, []);
  const toggleNowPlaying = useCallback(() => {
    setActiveRightPanel((p) => (p === "nowPlaying" ? null : "nowPlaying"));
  }, []);
  const toggleLyrics = useCallback(() => {
    setActiveRightPanel((p) => (p === "lyrics" ? null : "lyrics"));
  }, []);
  const toggleDeviceMenu = useCallback(
    () => setIsDeviceMenuOpen((p) => !p),
    [],
  );

  // Open the immersive view, remembering which entry point triggered
  // it (cover thumbnail / now-playing button vs the lyrics button). The
  // merged two-column ImmersiveView is self-contained — it fetches its
  // own lyrics via `useTrackLyrics` — so opening it no longer needs to
  // force the right-edge LyricsPanel open the way the old karaoke
  // overlay did.
  const openImmersive = useCallback((tab: "nowPlaying" | "lyrics") => {
    setImmersiveInitialTab(tab);
    setImmersiveOpen(true);
  }, []);
  const closeImmersive = useCallback(() => {
    setImmersiveOpen(false);
  }, []);
  // Back-compat aliases — callsites (PlayerBar cover action, LyricsPanel
  // maximize button) keep their existing names; both now open/close the
  // one merged immersive view.
  const openFullscreenNowPlaying = useCallback(
    () => openImmersive("nowPlaying"),
    [openImmersive],
  );
  const openFullscreenLyrics = useCallback(
    () => openImmersive("lyrics"),
    [openImmersive],
  );
  const closeFullscreenNowPlaying = closeImmersive;
  const closeFullscreenLyrics = closeImmersive;

  const refreshOutputDevices = useCallback(async () => {
    try {
      const list = await playerListOutputDevices();
      setOutputDevices(list);
    } catch (err) {
      console.error("[PlayerContext] refresh output devices failed", err);
    }
  }, []);

  // Background-prime the Deezer/Last.fm cache for the current artist.
  // This used to live in NowPlayingPanel, but that only fired when the
  // panel was open — closing it meant the artist grid in LibraryView
  // never got a picture for tracks the user actually played. Calling
  // it here is cheap (the backend cache hit short-circuits the network
  // path within ~10 ms when the row is fresh).
  useEffect(() => {
    if (activeProvider === "spotify") return;
    const artistId = currentTrack?.artist_id;
    if (artistId == null) return;
    enrichArtistDeezer(artistId).catch(() => {});
  }, [activeProvider, currentTrack?.artist_id]);

  useEffect(() => {
    if (activeProvider !== "spotify") return;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setPlaybackState(spotify.playbackState);
    setPositionMs(spotify.positionMs);
    setDurationMs(spotify.durationMs);
    if (spotify.currentTrack) {
      setCurrentTrack(spotifyTrackToTrack(spotify.currentTrack));
    }
    setVolumeState(spotify.volume);
  }, [
    activeProvider,
    spotify.playbackState,
    spotify.positionMs,
    spotify.durationMs,
    spotify.currentTrack,
    spotify.volume,
  ]);

  // Pre-fetch the device list at mount. Re-runs on profile switch
  // because `current_output_device` is per-profile (the persisted
  // pick lives in `profile_setting`), so the active row in the
  // cached list could change when the user swaps profiles.
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void refreshOutputDevices();
  }, [activeProfile, refreshOutputDevices]);

  return (
    <PlayerContext.Provider
      value={{
        activeRightPanel,
        isQueueOpen,
        toggleQueue,
        isNowPlayingOpen,
        toggleNowPlaying,
        isLyricsOpen,
        toggleLyrics,
        isDeviceMenuOpen,
        toggleDeviceMenu,
        immersiveOpen,
        immersiveInitialTab,
        openImmersive,
        closeImmersive,
        openFullscreenNowPlaying,
        closeFullscreenNowPlaying,
        openFullscreenLyrics,
        closeFullscreenLyrics,
        outputDevices,
        refreshOutputDevices,
        activeProvider,
        playbackState: effectivePlaybackState,
        isPlaying,
        currentTrack,
        currentRadioStation,
        positionMs,
        durationMs,
        volume,
        setVolume,
        toggleMute,
        playbackSpeed,
        setPlaybackSpeed,
        deviceSampleRate,
        deviceChannels,
        isShuffled,
        toggleShuffle,
        repeatMode,
        cycleRepeatMode,
        playTracks,
        playSpotifyTrack,
        playSpotifyContext,
        togglePlayback,
        next,
        previous,
        seek,
        setSeeking,
      }}
    >
      {children}
    </PlayerContext.Provider>
  );
}

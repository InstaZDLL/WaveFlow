import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { SpotifyContext, type SpotifyPlaybackState } from "../hooks/useSpotify";
import {
  spotifyGetAccessToken,
  spotifyGetPlaylistTracks,
  spotifyGetStatus,
  spotifyLogin,
  spotifyLogout,
  spotifyPauseLocal,
  type SpotifyPlaylistLite,
  type SpotifyStatus,
  type SpotifyTrackLite,
} from "../lib/tauri/spotify";

type SpotifySdkPlayer = {
  connect: () => Promise<boolean>;
  disconnect: () => void;
  addListener: (event: string, cb: (payload: unknown) => void) => boolean;
  togglePlay: () => Promise<void>;
  nextTrack: () => Promise<void>;
  previousTrack: () => Promise<void>;
  seek: (positionMs: number) => Promise<void>;
  setVolume: (volume01: number) => Promise<void>;
};

type SpotifySdk = {
  Player: new (options: {
    name: string;
    getOAuthToken: (cb: (token: string) => void) => void;
    volume?: number;
  }) => SpotifySdkPlayer;
};

type SpotifyWebPlaybackState = {
  paused: boolean;
  position: number;
  duration: number;
  track_window: {
    current_track: {
      id: string;
      name: string;
      uri: string;
      duration_ms: number;
      artists: { name: string }[];
      album: {
        name: string;
        images: { url: string }[];
      };
    } | null;
  };
};

declare global {
  interface Window {
    Spotify?: SpotifySdk;
    onSpotifyWebPlaybackSDKReady?: () => void;
  }
}

const SDK_SRC = "https://sdk.scdn.co/spotify-player.js";

function sdkTrackToLite(
  track: SpotifyWebPlaybackState["track_window"]["current_track"],
): SpotifyTrackLite | null {
  if (!track) return null;
  return {
    id: track.id,
    name: track.name,
    uri: track.uri,
    duration_ms: track.duration_ms,
    explicit: false,
    artist_name: track.artists[0]?.name ?? null,
    album_name: track.album.name,
    image_url: track.album.images[0]?.url ?? null,
  };
}

// True when this provider runs inside the mini-player webview.
// The Web Playback SDK can only attach to a single webview (it's a
// real device on the Spotify network), so the mini reads state from
// Tauri events emitted by the main window instead and routes
// playback control through the Spotify Connect Web API.
const IS_MINI_WINDOW =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("mini") === "1";

const SPOTIFY_STATE_EVENT = "spotify:state";
/// Mini → main: "I just opened, please rebroadcast your current
/// state so I'm not blank until the next track-changed callback."
const SPOTIFY_REQUEST_STATE_EVENT = "spotify:request-state";

interface SpotifyStateEvent {
  current_track: SpotifyTrackLite | null;
  position_ms: number;
  duration_ms: number;
  playback_state: SpotifyPlaybackState;
  volume: number;
}

export function SpotifyProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<SpotifyStatus | null>(null);
  const [isSdkReady, setIsSdkReady] = useState(false);
  const [deviceId, setDeviceId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [currentTrack, setCurrentTrack] = useState<SpotifyTrackLite | null>(
    null,
  );
  const [playbackState, setPlaybackState] =
    useState<SpotifyPlaybackState>("idle");
  const [positionMs, setPositionMs] = useState(0);
  const [durationMs, setDurationMs] = useState(0);
  const [volume, setVolumeState] = useState(80);
  const playerRef = useRef<SpotifySdkPlayer | null>(null);
  const positionTimerRef = useRef<number | null>(null);
  const initialVolumeRef = useRef(volume);
  // Latest broadcast snapshot kept in a ref so the request-state
  // handler can replay it without going through React state (avoids
  // a render-cycle delay between request and reply).
  const lastBroadcastRef = useRef<SpotifyStateEvent | null>(null);

  const isConnected = !!status?.connected;

  const refreshStatus = useCallback(async () => {
    const next = await spotifyGetStatus();
    setStatus(next);
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    refreshStatus().catch((err) =>
      console.error("[SpotifyContext] status failed", err),
    );
  }, [refreshStatus]);

  useEffect(() => {
    if (!isConnected) return;
    // Mini-player webview never loads the SDK — see IS_MINI_WINDOW
    // doc above.
    if (IS_MINI_WINDOW) return;
    if (window.Spotify) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setIsSdkReady(true);
      return;
    }
    const existing = document.querySelector<HTMLScriptElement>(
      `script[src="${SDK_SRC}"]`,
    );
    window.onSpotifyWebPlaybackSDKReady = () => setIsSdkReady(true);
    if (existing) return;
    const script = document.createElement("script");
    script.src = SDK_SRC;
    script.async = true;
    script.onerror = () => {
      setError("Spotify Web Playback SDK is unavailable on this platform.");
    };
    document.body.appendChild(script);
  }, [isConnected]);

  useEffect(() => {
    if (!isConnected || !isSdkReady || !window.Spotify || playerRef.current) {
      return;
    }
    const player = new window.Spotify.Player({
      name: "WaveFlow",
      volume: initialVolumeRef.current / 100,
      getOAuthToken: (cb) => {
        spotifyGetAccessToken()
          .then((token) => cb(token.access_token))
          .catch((err) => {
            console.error("[SpotifyContext] token failed", err);
            setError(String(err));
          });
      },
    });

    player.addListener("ready", (payload) => {
      const ready = payload as { device_id: string };
      setDeviceId(ready.device_id);
      setError(null);
    });
    player.addListener("not_ready", () => {
      setDeviceId(null);
      setError("Spotify device is not ready.");
    });
    player.addListener("authentication_error", (payload) => {
      const err = payload as { message?: string };
      setError(err.message ?? "Spotify authentication failed.");
    });
    player.addListener("account_error", (payload) => {
      const err = payload as { message?: string };
      setError(err.message ?? "Spotify Premium is required.");
    });
    player.addListener("initialization_error", (payload) => {
      const err = payload as { message?: string };
      setError(err.message ?? "Spotify SDK initialization failed.");
    });
    player.addListener("player_state_changed", (payload) => {
      const state = payload as SpotifyWebPlaybackState | null;
      if (!state) return;
      const track = sdkTrackToLite(state.track_window.current_track);
      const playback: SpotifyPlaybackState = state.paused ? "paused" : "playing";
      setCurrentTrack(track);
      setPositionMs(state.position);
      setDurationMs(state.duration);
      setPlaybackState(playback);
      // Broadcast to other Tauri windows (mini-player) — the SDK
      // attaches to a single webview but every WaveFlow window needs
      // to know what's playing.
      const snapshot: SpotifyStateEvent = {
        current_track: track,
        position_ms: state.position,
        duration_ms: state.duration,
        playback_state: playback,
        volume,
      };
      lastBroadcastRef.current = snapshot;
      import("@tauri-apps/api/event")
        .then(({ emit }) => emit(SPOTIFY_STATE_EVENT, snapshot))
        .catch(() => {});
    });

    playerRef.current = player;
    player.connect().then((connected) => {
      if (!connected) {
        setError("Spotify Web Playback SDK failed to connect.");
      }
    });
    return () => {
      player.disconnect();
      playerRef.current = null;
      setDeviceId(null);
    };
  }, [isConnected, isSdkReady, volume]);

  // Mini-player webview: subscribe to the broadcast emitted by the
  // main window's SDK callback so the mini stays in sync without
  // running a second SDK instance.
  useEffect(() => {
    if (!IS_MINI_WINDOW) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    (async () => {
      const { listen, emit } = await import("@tauri-apps/api/event");
      const off = await listen<SpotifyStateEvent>(SPOTIFY_STATE_EVENT, (e) => {
        const s = e.payload;
        setCurrentTrack(s.current_track);
        setPositionMs(s.position_ms);
        setDurationMs(s.duration_ms);
        setPlaybackState(s.playback_state);
        setVolumeState(s.volume);
      });
      if (cancelled) {
        off();
      } else {
        unlisten = off;
        // Ask the main window to replay its last broadcast — without
        // this, the mini stays blank until the next player_state_changed
        // callback fires (which only happens on play/pause/seek/track
        // change). 250 ms is a generous slack for the listener on the
        // main side to be wired up before we fire the request.
        setTimeout(() => {
          emit(SPOTIFY_REQUEST_STATE_EVENT, {}).catch(() => {});
        }, 250);
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Main window: respond to the mini's request-state pings by
  // rebroadcasting our last known snapshot (when we have one).
  useEffect(() => {
    if (IS_MINI_WINDOW) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    (async () => {
      const { listen, emit } = await import("@tauri-apps/api/event");
      const off = await listen(SPOTIFY_REQUEST_STATE_EVENT, () => {
        const snapshot = lastBroadcastRef.current;
        if (snapshot) {
          emit(SPOTIFY_STATE_EVENT, snapshot).catch(() => {});
        }
      });
      if (cancelled) off();
      else unlisten = off;
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    if (positionTimerRef.current != null) {
      window.clearInterval(positionTimerRef.current);
      positionTimerRef.current = null;
    }
    if (playbackState !== "playing") return;
    positionTimerRef.current = window.setInterval(() => {
      setPositionMs((current) => Math.min(current + 1000, durationMs));
    }, 1000);
    return () => {
      if (positionTimerRef.current != null) {
        window.clearInterval(positionTimerRef.current);
        positionTimerRef.current = null;
      }
    };
  }, [playbackState, durationMs]);

  const tokenHeaders = useCallback(async () => {
    const token = await spotifyGetAccessToken();
    return {
      Authorization: `Bearer ${token.access_token}`,
      "Content-Type": "application/json",
    };
  }, []);

  const transferPlayback = useCallback(async () => {
    if (!deviceId) throw new Error("Spotify device is not ready.");
    const headers = await tokenHeaders();
    await fetch("https://api.spotify.com/v1/me/player", {
      method: "PUT",
      headers,
      body: JSON.stringify({ device_ids: [deviceId], play: false }),
    });
  }, [deviceId, tokenHeaders]);

  const playRequest = useCallback(
    async (body: Record<string, unknown>) => {
      if (!deviceId) throw new Error("Spotify device is not ready.");
      setPlaybackState("loading");
      setError(null);
      await spotifyPauseLocal();
      await transferPlayback();
      const headers = await tokenHeaders();
      const res = await fetch(
        `https://api.spotify.com/v1/me/player/play?device_id=${encodeURIComponent(deviceId)}`,
        {
          method: "PUT",
          headers,
          body: JSON.stringify(body),
        },
      );
      if (!res.ok && res.status !== 204) {
        const text = await res.text();
        throw new Error(text || `Spotify play failed (${res.status})`);
      }
    },
    [deviceId, tokenHeaders, transferPlayback],
  );

  const login = useCallback(async () => {
    const next = await spotifyLogin();
    setStatus(next);
  }, []);

  const logout = useCallback(async () => {
    playerRef.current?.disconnect();
    playerRef.current = null;
    setDeviceId(null);
    setCurrentTrack(null);
    setPlaybackState("idle");
    await spotifyLogout();
    await refreshStatus();
  }, [refreshStatus]);

  const playTrack = useCallback(
    async (track: SpotifyTrackLite) => {
      setCurrentTrack(track);
      setDurationMs(track.duration_ms);
      setPositionMs(0);
      await playRequest({ uris: [track.uri] });
    },
    [playRequest],
  );

  const playContext = useCallback(
    async (contextUri: string) => {
      await playRequest({ context_uri: contextUri });
    },
    [playRequest],
  );

  // Mini-player webview has no SDK player attached, so its controls
  // route through the Spotify Connect Web API instead. Same for any
  // window once the SDK fails to attach (e.g. user blocked mixed
  // content). The connect/play/pause/seek/next/previous endpoints
  // act on whichever device is currently active on the user's
  // account, which is the SDK-attached main window.
  const connectApi = useCallback(
    async (
      method: "PUT" | "POST",
      endpoint: string,
      body?: Record<string, unknown>,
    ) => {
      const headers = await tokenHeaders();
      const init: RequestInit = { method, headers };
      if (body) init.body = JSON.stringify(body);
      const res = await fetch(`https://api.spotify.com/v1/me/player/${endpoint}`, init);
      if (!res.ok && res.status !== 204) {
        const text = await res.text();
        throw new Error(text || `Spotify ${endpoint} failed (${res.status})`);
      }
    },
    [tokenHeaders],
  );

  const togglePlayback = useCallback(async () => {
    if (playerRef.current) {
      await playerRef.current.togglePlay();
    } else {
      // No SDK locally — flip via Connect API. The current state
      // tells us which endpoint to hit.
      await connectApi("PUT", playbackState === "playing" ? "pause" : "play");
    }
  }, [connectApi, playbackState]);

  const next = useCallback(async () => {
    if (playerRef.current) await playerRef.current.nextTrack();
    else await connectApi("POST", "next");
  }, [connectApi]);

  const previous = useCallback(async () => {
    if (playerRef.current) await playerRef.current.previousTrack();
    else await connectApi("POST", "previous");
  }, [connectApi]);

  const seek = useCallback(
    async (ms: number) => {
      setPositionMs(ms);
      if (playerRef.current) {
        await playerRef.current.seek(ms);
      } else {
        await connectApi("PUT", `seek?position_ms=${Math.max(0, Math.floor(ms))}`);
      }
    },
    [connectApi],
  );

  const setVolume = useCallback(
    async (value: number) => {
      const clamped = Math.max(0, Math.min(100, Math.round(value)));
      setVolumeState(clamped);
      if (playerRef.current) {
        await playerRef.current.setVolume(clamped / 100);
      } else {
        await connectApi("PUT", `volume?volume_percent=${clamped}`);
      }
    },
    [connectApi],
  );

  const loadPlaylistTracks = useCallback(
    (playlist: SpotifyPlaylistLite) => spotifyGetPlaylistTracks(playlist.id),
    [],
  );

  const value = useMemo(
    () => ({
      status,
      isConnected,
      isSdkReady,
      deviceId,
      error,
      currentTrack,
      playbackState,
      positionMs,
      durationMs,
      volume,
      refreshStatus,
      login,
      logout,
      playTrack,
      playContext,
      togglePlayback,
      next,
      previous,
      seek,
      setVolume,
      loadPlaylistTracks,
    }),
    [
      status,
      isConnected,
      isSdkReady,
      deviceId,
      error,
      currentTrack,
      playbackState,
      positionMs,
      durationMs,
      volume,
      refreshStatus,
      login,
      logout,
      playTrack,
      playContext,
      togglePlayback,
      next,
      previous,
      seek,
      setVolume,
      loadPlaylistTracks,
    ],
  );

  return (
    <SpotifyContext.Provider value={value}>{children}</SpotifyContext.Provider>
  );
}

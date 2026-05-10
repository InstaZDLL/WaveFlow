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
      setCurrentTrack(sdkTrackToLite(state.track_window.current_track));
      setPositionMs(state.position);
      setDurationMs(state.duration);
      setPlaybackState(state.paused ? "paused" : "playing");
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
  }, [isConnected, isSdkReady]);

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

  const togglePlayback = useCallback(async () => {
    await playerRef.current?.togglePlay();
  }, []);

  const next = useCallback(async () => {
    await playerRef.current?.nextTrack();
  }, []);

  const previous = useCallback(async () => {
    await playerRef.current?.previousTrack();
  }, []);

  const seek = useCallback(async (ms: number) => {
    setPositionMs(ms);
    await playerRef.current?.seek(ms);
  }, []);

  const setVolume = useCallback(async (value: number) => {
    const clamped = Math.max(0, Math.min(100, Math.round(value)));
    setVolumeState(clamped);
    await playerRef.current?.setVolume(clamped / 100);
  }, []);

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

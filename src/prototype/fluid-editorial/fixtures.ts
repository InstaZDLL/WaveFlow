/**
 * Typed view over `fixtures.generated.json`, produced by
 * `scripts/fluid-proto-fixtures.ts` from a real profile database.
 *
 * The covers it references live under `prototypes/fluid-editorial/covers/`,
 * a gitignored folder (album art is the user's). Every consumer has a
 * palette to fall back on, so the page still composes on a machine
 * without the images — flatter, but laid out the same.
 */
import raw from "./fixtures.generated.json";

export interface FeAlbum {
  id: number;
  title: string;
  artist: string;
  year: number | null;
  trackCount: number;
  plays: number;
  lastPlayed: number | null;
  addedAt: number;
  genre: string | null;
  cover: string | null;
  palette: string[] | null;
}

export interface FeTrack {
  trackId: number;
  title: string;
  artist: string;
  albumId: number | null;
  album: string | null;
  durationMs: number;
  playedAt: number;
  codec: string | null;
  sampleRate: number | null;
  bitDepth: number | null;
  bpm: number | null;
  energy: number | null;
  cover: string | null;
  palette: string[] | null;
}

export type SmartKind =
  "daily_mix" | "on_repeat" | "artist_mix" | "genre_mix" | "decade_mix";

export interface FePlaylist {
  id: number;
  name: string;
  isSmart: boolean;
  smartKind: SmartKind | null;
  colorId: string;
  iconId: string;
  trackCount: number;
  covers: string[];
}

export interface FeStats {
  tracks: number;
  albums: number;
  artists: number;
  liked: number;
  plays: number;
  playlists: number;
  analysed: number;
  listenedMs: number;
}

export type MoodId = "sleep" | "focus" | "chill" | "party" | "workout";

interface RawFixtures {
  stats: {
    tracks: number;
    albums: number;
    artists: number;
    liked: number;
    plays: number;
    playlists: number;
    analysed: number;
    listened_ms: number;
  };
  moodCounts: Record<"focus" | "chill" | "workout" | "party" | "sleep", number>;
  lastPlayed: FeAlbum[];
  topAlbums: FeAlbum[];
  recentAlbums: FeAlbum[];
  recentPlays: FeTrack[];
  playlists: Array<
    Omit<FePlaylist, "smartKind"> & { smartRules: string | null }
  >;
}

const data = raw as unknown as RawFixtures;

function smartKind(rules: string | null): SmartKind | null {
  if (!rules) return null;
  try {
    const kind = (JSON.parse(rules) as { kind?: string }).kind;
    switch (kind) {
      case "daily_mix":
      case "on_repeat":
      case "artist_mix":
      case "genre_mix":
      case "decade_mix":
        return kind;
      default:
        return null;
    }
  } catch {
    return null;
  }
}

/** Drop a track that has no album row at all — it cannot be composed
 *  into anything on this page (the untitled WAV in the sample). */
const plays = data.recentPlays.filter((p) => p.album != null);
if (plays.length === 0) {
  // The generator refuses to write such a file; a hand-edited one
  // should fail here, not on `recentPlays[0].durationMs` in `state.tsx`.
  throw new Error(
    "fixtures.generated.json has no play with an album — regenerate it with scripts/fluid-proto-fixtures.ts",
  );
}

export const fixtures = {
  stats: {
    tracks: data.stats.tracks,
    albums: data.stats.albums,
    artists: data.stats.artists,
    liked: data.stats.liked,
    plays: data.stats.plays,
    playlists: data.stats.playlists,
    analysed: data.stats.analysed,
    listenedMs: data.stats.listened_ms,
  } satisfies FeStats,
  moodCounts: data.moodCounts,
  /** Most recent first. */
  recentPlays: plays,
  lastPlayedAlbums: data.lastPlayed,
  topAlbums: data.topAlbums,
  recentAlbums: data.recentAlbums,
  playlists: data.playlists.map<FePlaylist>((p) => ({
    id: p.id,
    name: p.name,
    isSmart: p.isSmart,
    smartKind: smartKind(p.smartRules),
    colorId: p.colorId,
    iconId: p.iconId,
    trackCount: p.trackCount,
    covers: p.covers,
  })),
};

export const albumById = new Map<number, FeAlbum>(
  [...data.topAlbums, ...data.recentAlbums, ...data.lastPlayed].map((a) => [
    a.id,
    a,
  ]),
);

/** A playable stand-in built from an album row, for tiles that start
 *  an album rather than a track. */
export function trackFromAlbum(album: FeAlbum): FeTrack {
  return {
    trackId: -album.id,
    title: album.title,
    artist: album.artist,
    albumId: album.id,
    album: album.title,
    durationMs: 214_000,
    playedAt: Date.now() / 1000,
    codec: "FLAC",
    sampleRate: 44_100,
    bitDepth: 16,
    bpm: null,
    energy: null,
    cover: album.cover,
    palette: album.palette,
  };
}

import { useEffect, useState, useSyncExternalStore } from "react";

import { fetchTrackCanvas, getTrackCanvas } from "../lib/tauri/canvas";
import { remoteTrackCanvas } from "../lib/tauri/remoteServer";
import { useProfile } from "./useProfile";

/**
 * Process-wide dedupe + cache for per-track Canvas lookups (issue #442),
 * mirroring `useAlbumMotionArtwork`.
 *
 * The hook is mounted by several surfaces at once (the immersive top bar
 * decides toggle visibility, `ImmersiveNowPlaying` renders the stage,
 * `NowPlayingPanel` does both), so a single track change would otherwise
 * fire the same `get_track_canvas` query three or four times. `inFlight`
 * collapses concurrent callers onto one promise; `resolved` keeps the
 * answer so remounting a panel or flipping back to a recent track is free.
 *
 * `resolved` is capped and evicted oldest-first so a long session doesn't
 * retain an entry per track played.
 */
const MAX_RESOLVED = 128;
const inFlight = new Map<number, Promise<string | null>>();
const resolved = new Map<number, string | null>();

function rememberResolved(trackId: number, value: string | null): void {
  resolved.delete(trackId);
  resolved.set(trackId, value);
  while (resolved.size > MAX_RESOLVED) {
    const oldest = resolved.keys().next();
    if (oldest.done) break;
    resolved.delete(oldest.value);
  }
}

// Per-trackId generation, bumped by `invalidateTrackCanvas`. A request
// captures the generation live at creation time; if a set/clear bumps it
// while the request is in flight, that request's result is stale and must
// neither be cached nor allowed to clear a newer request's inFlight entry.
const generation = new Map<number, number>();
function generationOf(trackId: number): number {
  return generation.get(trackId) ?? 0;
}

/** The track fields the Canvas lookup needs: `id` keys the manual local
 *  Canvas + all caching; the rest let the plugin fallback (issue #473)
 *  resolve a Canvas against an external catalogue. A {@link Track} (e.g.
 *  `currentTrack`) is structurally assignable. */
export interface CanvasTrackInput {
  id: number;
  title: string;
  artist_name: string | null;
  album_title: string | null;
  duration_ms: number;
  /** Set only for a track playing from the bound server; absent is local. */
  remote_id?: string;
}

/**
 * Resolve a track's Canvas source. The **manual local mp4** wins; failing
 * that, ask enabled `canvas`-world plugins for a **remote** one (issue
 * #473). Returns either a local absolute path OR an `https` URL — the
 * consumer's `CanvasStage` tells them apart — or `null` when neither
 * yields a Canvas.
 */
async function resolveCanvasSource(
  track: CanvasTrackInput,
): Promise<string | null> {
  // A server track takes the rung the manual local clip holds, and for the
  // same reason: somebody put it on that library on purpose. It cannot use
  // the local lookup at all — `track.id` is a negative sentinel here, so
  // `get_track_canvas` would be asking the local database about a rowid that
  // does not exist.
  if (track.remote_id) {
    const remote = await remoteTrackCanvas(track.remote_id);
    if (remote) return remote;
    // No Canvas on the server is an ordinary answer; fall through to the
    // plugin, which resolves by artist and title and knows nothing about
    // where the track is stored.
  } else {
    const manual = await getTrackCanvas(track.id);
    if (manual?.localPath) return manual.localPath;
  }
  // No manual clip — fall back to a plugin. It needs artist + title to
  // resolve against an external source; skip when either is missing.
  if (!track.artist_name || !track.title) return null;
  const plugin = await fetchTrackCanvas(
    track.artist_name,
    track.title,
    track.album_title,
    track.duration_ms,
  );
  return plugin?.url ?? null;
}

function lookup(track: CanvasTrackInput): Promise<string | null> {
  const trackId = track.id;
  if (resolved.has(trackId)) {
    return Promise.resolve(resolved.get(trackId) ?? null);
  }
  const pending = inFlight.get(trackId);
  if (pending) return pending;

  const myGeneration = generationOf(trackId);
  const myProfileGen = profileGeneration;
  const request: Promise<string | null> = resolveCanvasSource(track)
    .then((src) => {
      // Only cache when neither a per-track invalidation (set/clear) NOR a
      // profile switch has happened since the request started — either makes
      // the answer stale, and a late completion from the previous profile
      // must never repopulate the cache for a colliding id.
      if (
        profileGeneration === myProfileGen &&
        generationOf(trackId) === myGeneration
      ) {
        rememberResolved(trackId, src);
      }
      return src;
    })
    // A failed lookup is NOT remembered, so a transient error does not
    // suppress the Canvas for the rest of the session — a Canvas is
    // decoration, and no failure here may break playback. Logged rather
    // than swallowed outright: "no Canvas" and "the lookup broke" look
    // identical on screen, and only one of them is worth investigating.
    .catch((err) => {
      console.debug("[useTrackCanvas] lookup failed", err);
      return null;
    })
    .finally(() => {
      // Only clear inFlight if we're still the registered request: an
      // invalidation may have replaced us with a newer one, which we must
      // not delete.
      if (inFlight.get(trackId) === request) inFlight.delete(trackId);
    });

  inFlight.set(trackId, request);
  return request;
}

// Invalidation signal: bumped whenever a Canvas is set/cleared so every
// mounted `useTrackCanvas` re-resolves, even for the same trackId (its
// effect deps wouldn't otherwise change). A plain epoch + listener set,
// read through `useSyncExternalStore`.
let epoch = 0;
const epochListeners = new Set<() => void>();
function subscribeEpoch(cb: () => void): () => void {
  epochListeners.add(cb);
  return () => epochListeners.delete(cb);
}
function getEpoch(): number {
  return epoch;
}

/**
 * Invalidate a track's cached Canvas path — call after setting or clearing
 * one so the mounted surfaces re-resolve instead of serving the stale
 * answer. Drops the cache entry and bumps the shared epoch to re-trigger
 * every `useTrackCanvas` effect.
 */
export function invalidateTrackCanvas(trackId: number): void {
  resolved.delete(trackId);
  inFlight.delete(trackId);
  // Bump the generation so any request already in flight for this track is
  // treated as stale when it resolves (won't overwrite the fresh answer).
  generation.set(trackId, generationOf(trackId) + 1);
  epoch += 1;
  for (const cb of epochListeners) cb();
}

// The caches above are keyed by `trackId` only, but track ids are per-profile
// (each profile has its own SQLite DB), so a colliding id must not serve
// another profile's Canvas after a switch. Track the active profile and drop
// the whole cache when it changes — track ids from the previous profile are
// all meaningless now, so a full clear is both correct and simplest.
//
// `profileGeneration` is a monotonic token (never reset): a request captures
// it at start, and a completion from a prior profile is rejected before it can
// repopulate the cache. Clearing the maps alone wouldn't suffice — a stale
// in-flight request would re-capture generation 0 and slip through.
let activeProfileId: number | null = null;
let profileGeneration = 0;
function resetCacheForProfile(profileId: number | null): void {
  if (profileId === activeProfileId) return;
  activeProfileId = profileId;
  profileGeneration += 1;
  resolved.clear();
  inFlight.clear();
  generation.clear();
  epoch += 1;
  for (const cb of epochListeners) cb();
}

/**
 * Resolve a track's Canvas clip local path, or `null` when it has none
 * (or the id is missing). setState only fires inside the promise callbacks
 * (never synchronously in the effect body — `react-hooks/set-state-in-effect`),
 * and a `cancelled` guard drops a stale in-flight result on a fast track
 * change.
 */
export function useTrackCanvas(
  track: CanvasTrackInput | null | undefined,
): string | null {
  // Store the resolved source together with the track id AND profile it
  // belongs to, so the render can gate on a match below — a bare path would
  // flash the previous track's (or previous profile's) Canvas for one render
  // after the inputs change but before the effect resolves.
  const [resolved, setResolved] = useState<{
    id: number;
    profileId: number | null;
    path: string | null;
  } | null>(null);
  // Re-run the effect when a set/clear bumps the epoch, even for the same
  // trackId — otherwise the surface would keep the stale answer.
  const currentEpoch = useSyncExternalStore(subscribeEpoch, getEpoch, getEpoch);

  // Drop the shared cache on a profile switch so a per-profile track id can't
  // reuse another profile's Canvas. Idempotent + guarded, so the several
  // mounted instances clear at most once per switch.
  const profileId = useProfile().activeProfile?.id ?? null;
  useEffect(() => {
    resetCacheForProfile(profileId);
  }, [profileId]);

  // Primitive deps so the effect re-runs when the track (or any field the
  // plugin fallback keys on) changes, without churning on a fresh object
  // identity every render.
  const trackId = track?.id ?? null;
  const artistName = track?.artist_name ?? null;
  const title = track?.title ?? null;
  const albumTitle = track?.album_title ?? null;
  const durationMs = track?.duration_ms ?? null;
  const remoteId = track?.remote_id ?? null;

  useEffect(() => {
    let cancelled = false;
    const apply = (p: string | null) => {
      if (!cancelled) setResolved({ id: trackId as number, profileId, path: p });
    };
    // Radio and Spotify play under a negative sentinel id: no library row
    // for a manual Canvas, and nothing meaningful to resolve a plugin one
    // against. A **server** track is also negative — the sentinel is minted
    // per playback — but it is not in that position: `remote_id` names it on
    // the server, which is what the remote lookup asks about. So the guard
    // is on having something to ask with, not on the sign of the id.
    const resolvable = trackId != null && (trackId >= 0 || remoteId != null);
    if (resolvable && title != null) {
      lookup({
        id: trackId,
        title,
        artist_name: artistName,
        album_title: albumTitle,
        duration_ms: durationMs ?? 0,
        ...(remoteId ? { remote_id: remoteId } : {}),
      }).then(apply, () => apply(null));
    }
    return () => {
      cancelled = true;
    };
    // `currentEpoch` is a deliberate dep: a bump forces a re-resolve.
  }, [
    trackId,
    remoteId,
    artistName,
    title,
    albumTitle,
    durationMs,
    currentEpoch,
    profileId,
  ]);

  // Only surface the source when it belongs to BOTH the currently-requested
  // track and the active profile; any mismatch (track or profile just changed,
  // effect not resolved yet) reads as null, so a previous track's/profile's
  // clip never bleeds onto the new one.
  return resolved &&
    resolved.id === trackId &&
    resolved.profileId === profileId
    ? resolved.path
    : null;
}

import { useSyncExternalStore } from "react";

import { getPlayerPosition } from "../hooks/usePlayerPosition";
import type { ClipSource } from "./tauri/clip";

/**
 * The music video on screen (#1073), if any — one at a time, and the
 * transport the player bar drives while it is shown.
 *
 * A store rather than view state because several places take part: the
 * now-playing panel and the immersive view show the video (one of them at a
 * time, [`ClipHost`](../components/player/ClipHost.tsx) decides which), the
 * player bar's progress and Play button drive it, and a track's context
 * menu or the panel's switch opens it. The `<video>` element itself is
 * registered here by whichever surface shows it, so a move from the panel
 * to the immersive view carries the position across.
 *
 * **Every opening is a session of its own** (`OpenClip.session`). What an
 * element reports, what an exit does after its awaits, which element the
 * transport drives: each is tied to the session it belongs to, and a
 * session that was replaced or closed is ignored. Without it, the clip
 * being left wrote its position into the next one, a late exit resumed the
 * track under a new clip, and opening a paused clip again did nothing.
 */
export interface OpenClip {
  /** A new number at every opening, the same clip again included. */
  session: number;
  /** The track's library id, or the sentinel a server track plays under
   *  while it is the one playing (`remoteTrackId` is what names it). */
  trackId: number;
  /** Set for a server track. Its `trackId` is minted per playback, so the
   *  server's id is what tells whether the clip is the playing track's. */
  remoteTrackId: string | null;
  source: ClipSource;
  title: string;
  artist: string | null;
  /** Where the clip starts, in ms: the track's position when it was
   *  switched to (the clip's own length bounds it once known). */
  startMs: number;
}

export type ClipFailure = "unplayable" | "gone" | "unreachable";

export interface ClipState {
  clip: OpenClip | null;
  positionMs: number;
  /** 0 until the video's metadata has loaded. */
  durationMs: number;
  paused: boolean;
  /** Why it is not playing, once it cannot: the webview could not read
   *  it, the server no longer has it, or the server could not be reached. */
  failed: ClipFailure | null;
  /** Width over height, once known: the panel's frame takes the clip's
   *  shape rather than the cover's square. */
  aspect: number | null;
}

const CLOSED: ClipState = {
  clip: null,
  positionMs: 0,
  durationMs: 0,
  paused: true,
  failed: null,
  aspect: null,
};

let state: ClipState = CLOSED;
let lastSession = 0;
/** Moves at every opening and every closing, never back. */
let epoch = 0;
/** The element showing the clip, and whether it has been put at the
 *  clip's position yet: until its metadata loads, its `currentTime` is 0
 *  and says nothing about where the clip is. */
let video: { session: number; element: HTMLVideoElement } | null = null;
let videoPositioned = false;
/** Elements already put at their clip's position. One element serves a
 *  whole session and moves between surfaces without reloading, so its
 *  metadata loads once: a surface that adopts it later finds it placed. */
const positionedElements = new WeakSet<HTMLVideoElement>();
const listeners = new Set<() => void>();

function set(next: Partial<ClipState>) {
  state = { ...state, ...next };
  listeners.forEach((listener) => listener());
}

/**
 * Takes over the transport while it must: a server clip whose ticket has run
 * out renews it before the element is asked to play or seek
 * ([`clipServerSource`](./clipServerSource.ts)). Each method answers whether
 * it took the request; a request it did not take goes to the element.
 */
export interface ClipTransportGuard {
  /** Whether the element is not to be trusted for "is it paused" now (its
   *  source is being replaced): the store's word is used instead. */
  busy(): boolean;
  play(): boolean;
  pause(): boolean;
  seek(ms: number): boolean;
}
let guard: { session: number; guard: ClipTransportGuard } | null = null;

/** Register the guard of `session`, or remove it with `null`. Dropped at
 *  every opening and closing too. */
export function guardClipTransport(
  session: number,
  next: ClipTransportGuard | null,
): void {
  if (next === null) {
    if (guard?.session === session) guard = null;
    return;
  }
  if (isCurrentClipSession(session)) guard = { session, guard: next };
}

function activeGuard(): ClipTransportGuard | null {
  return guard && isCurrentClipSession(guard.session) ? guard.guard : null;
}

/** Whether `session` is the clip on screen. */
export function isCurrentClipSession(session: number): boolean {
  return state.clip?.session === session;
}

/** Show a clip, as a new session. A clip already open is replaced, the same
 *  one included: opening it again starts it again. */
export function openClip(clip: Omit<OpenClip, "session">): void {
  lastSession += 1;
  epoch += 1;
  video = null;
  videoPositioned = false;
  guard = null;
  set({
    ...CLOSED,
    clip: { ...clip, session: lastSession },
    positionMs: clip.startMs,
    paused: false,
  });
}

/**
 * Switch `track` to its clip. The clip starts where the track is when it is
 * the one playing — the same moment of the song, as far as the two line up
 * — and at its start for any other track (a context menu can name one).
 */
export function openClipForTrack(
  track: ClipTrack & { title: string; artist_name: string | null },
  source: ClipSource,
  currentTrack: ClipTrack | null,
): void {
  const playing = currentTrack !== null && isSameTrack(track, currentTrack);
  openClip({
    // The playing track's own id when it is the one: a server row in a list
    // carries an id of its own, not the sentinel the track plays under.
    trackId: playing ? currentTrack.id : track.id,
    remoteTrackId: track.remote_id ?? null,
    source,
    title: track.title,
    artist: track.artist_name,
    startMs: playing ? getPlayerPosition() : 0,
  });
}

/** What names a track, for a clip: its id, and a server track's own id. */
export interface ClipTrack {
  id: number;
  remote_id?: string;
}

/** Whether two rows are the same track: a server track by the server's id,
 *  since each playback mints it a new sentinel; a library one by its id. */
function isSameTrack(a: ClipTrack, b: ClipTrack): boolean {
  if (a.remote_id !== undefined || b.remote_id !== undefined) {
    return a.remote_id === b.remote_id;
  }
  return a.id === b.id;
}

/** Whether `clip` is `track`'s, by the rule above. */
export function isClipOfTrack(
  clip: OpenClip,
  track: ClipTrack | null,
): boolean {
  if (track === null) return false;
  return isSameTrack(
    {
      id: clip.trackId,
      ...(clip.remoteTrackId !== null ? { remote_id: clip.remoteTrackId } : {}),
    },
    track,
  );
}

/** Take the clip off the screen. Going back to the track's audio — where
 *  it resumes, and from which position — is
 *  [`ClipHost`](../components/player/ClipHost.tsx)'s business: call
 *  [`requestBackToAudio`] for that. */
export function closeClip(): void {
  if (state.clip === null) return;
  epoch += 1;
  video = null;
  videoPositioned = false;
  guard = null;
  set(CLOSED);
}

/**
 * A counter that moves at every opening and every closing of a clip. Work
 * that awaits on behalf of a clip captures it before and compares it after:
 * any change means another visit started (and maybe ended) meanwhile, which
 * a check of "is a clip open now" would miss when it has ended too.
 */
export function clipEpoch(): number {
  return epoch;
}

/** How the clip is being left, for `ClipHost`. */
export type ClipExit = "user" | "ended";
type ExitHandler = (exit: ClipExit, positionMs: number) => void;
let exitHandler: ExitHandler | null = null;

/** `ClipHost` registers how a clip is left: the track's audio picks up
 *  where the clip was. */
export function onClipExit(handler: ExitHandler | null): void {
  exitHandler = handler;
}

/** The clip's position: the element's once it has been put there, the
 *  store's before (a switch back before the metadata loaded, or a format
 *  that failed, keeps the position the clip was opened at). */
function currentPositionMs(): number {
  if (video && videoPositioned && isCurrentClipSession(video.session)) {
    return Math.round(video.element.currentTime * 1000);
  }
  return state.positionMs;
}

/** Back to the track's audio, at the clip's position. */
export function requestBackToAudio(exit: ClipExit = "user"): void {
  if (state.clip === null) return;
  const positionMs = currentPositionMs();
  if (exitHandler) exitHandler(exit, positionMs);
  else closeClip();
}

/**
 * The surface showing the clip hands its `<video>` over, for `session`. The
 * session's one element moves between surfaces as it plays, so a move does
 * not restart it.
 */
export function attachClipVideo(
  session: number,
  element: HTMLVideoElement,
): void {
  if (!isCurrentClipSession(session)) return;
  video = { session, element };
  videoPositioned = positionedElements.has(element);
}

/** The surface lets go of its element. Its position is kept for the next
 *  surface, when the element had reached it. */
export function detachClipVideo(
  session: number,
  element: HTMLVideoElement,
): void {
  if (video?.element !== element) return;
  if (videoPositioned && isCurrentClipSession(session)) {
    set({ positionMs: Math.round(element.currentTime * 1000) });
  }
  video = null;
  videoPositioned = false;
}

/** The element of `session` now stands at the clip's position. */
export function markClipPositioned(
  session: number,
  element: HTMLVideoElement,
): void {
  if (!isCurrentClipSession(session)) return;
  positionedElements.add(element);
  if (video?.element === element) videoPositioned = true;
}

/** The element of `session` is loading a new source (a renewed ticket): its
 *  time says nothing about the clip until its metadata loads again, so the
 *  store's position is the clip's meanwhile. */
export function unmarkClipPositioned(
  session: number,
  element: HTMLVideoElement,
): void {
  positionedElements.delete(element);
  if (video?.element === element && isCurrentClipSession(session)) {
    videoPositioned = false;
  }
}

/** What the element of `session` reports, for the transport. Ignored once
 *  that session is no longer the clip on screen. */
export function reportClipProgress(
  session: number,
  next: Partial<ClipState>,
): void {
  if (!isCurrentClipSession(session)) return;
  set(next);
}

export function clipState(): ClipState {
  return state;
}

function element(): HTMLVideoElement | null {
  return video && isCurrentClipSession(video.session) ? video.element : null;
}

/**
 * Play the element, and say so in the store when the webview refuses
 * (autoplay with sound, `NotAllowedError`): the Play button then reads
 * Play, not Pause. Only a refusal of the element still showing the clip
 * counts. A play interrupted (`AbortError`) is no refusal: a move to the
 * other surface reloads the outgoing element under a pending `play()` (the
 * Linux loopback does), and the incoming element of the same session must
 * keep playing; a `pause()` that interrupts one reports itself through the
 * element's own `pause` event.
 */
export function playClipElement(session: number, target: HTMLVideoElement) {
  void target.play().catch((err: unknown) => {
    if (err instanceof DOMException && err.name === "AbortError") return;
    if (video?.element !== target) return;
    reportClipProgress(session, { paused: true });
  });
}

export function toggleClip(): void {
  const target = element();
  if (!target || !state.clip) return;
  const paused = activeGuard()?.busy() ? state.paused : target.paused;
  if (paused) playClip();
  else pauseClip();
}

export function playClip(): void {
  const target = element();
  if (!target || !state.clip) return;
  if (activeGuard()?.play()) return;
  playClipElement(state.clip.session, target);
}

export function pauseClip(): void {
  if (activeGuard()?.pause()) return;
  element()?.pause();
}

/** Seek the clip, in ms, inside its length. */
export function seekClip(ms: number): void {
  const target = element();
  if (!target) return;
  const duration = Number.isFinite(target.duration) ? target.duration : 0;
  const seconds = Math.max(0, ms / 1000);
  const bounded = duration > 0 ? Math.min(seconds, duration) : seconds;
  if (activeGuard()?.seek(Math.round(bounded * 1000))) return;
  target.currentTime = bounded;
  set({ positionMs: Math.round(target.currentTime * 1000) });
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useClipState(): ClipState {
  return useSyncExternalStore(
    subscribe,
    () => state,
    () => CLOSED,
  );
}

export function useOpenClip(): OpenClip | null {
  return useClipState().clip;
}

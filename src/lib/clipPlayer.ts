import { useSyncExternalStore } from "react";

import { getPlayerPosition } from "../hooks/usePlayerPosition";

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
  trackId: number;
  localPath: string;
  title: string;
  artist: string | null;
  /** Where the clip starts, in ms: the track's position when it was
   *  switched to (the clip's own length bounds it once known). */
  startMs: number;
}

export interface ClipState {
  clip: OpenClip | null;
  positionMs: number;
  /** 0 until the video's metadata has loaded. */
  durationMs: number;
  paused: boolean;
  /** The webview could not play it. */
  failed: boolean;
  /** Width over height, once known: the panel's frame takes the clip's
   *  shape rather than the cover's square. */
  aspect: number | null;
}

const CLOSED: ClipState = {
  clip: null,
  positionMs: 0,
  durationMs: 0,
  paused: true,
  failed: false,
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
  track: { id: number; title: string; artist_name: string | null },
  localPath: string,
  currentTrackId: number | null,
): void {
  openClip({
    trackId: track.id,
    localPath,
    title: track.title,
    artist: track.artist_name,
    startMs: track.id === currentTrackId ? getPlayerPosition() : 0,
  });
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
  if (target.paused) playClipElement(state.clip.session, target);
  else target.pause();
}

export function playClip(): void {
  const target = element();
  if (target && state.clip) playClipElement(state.clip.session, target);
}

export function pauseClip(): void {
  element()?.pause();
}

/** Seek the clip, in ms, inside its length. */
export function seekClip(ms: number): void {
  const target = element();
  if (!target) return;
  const duration = Number.isFinite(target.duration) ? target.duration : 0;
  const seconds = Math.max(0, ms / 1000);
  target.currentTime = duration > 0 ? Math.min(seconds, duration) : seconds;
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

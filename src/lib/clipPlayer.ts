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
 */
export interface OpenClip {
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
let video: HTMLVideoElement | null = null;
const listeners = new Set<() => void>();

function set(next: Partial<ClipState>) {
  state = { ...state, ...next };
  listeners.forEach((listener) => listener());
}

/** Show `clip`. A clip already open is replaced. */
export function openClip(clip: OpenClip): void {
  set({ ...CLOSED, clip, positionMs: clip.startMs, paused: false });
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
  video = null;
  set(CLOSED);
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

/** Back to the track's audio, at the clip's position. */
export function requestBackToAudio(exit: ClipExit = "user"): void {
  if (state.clip === null) return;
  const positionMs = video
    ? Math.round(video.currentTime * 1000)
    : state.positionMs;
  if (exitHandler) exitHandler(exit, positionMs);
  else closeClip();
}

/**
 * The surface showing the clip hands its `<video>` over (and `null` when it
 * stops showing it). The element is resumed at the clip's position and in
 * its play state, so a move between surfaces does not restart it.
 */
export function attachClipVideo(element: HTMLVideoElement | null): void {
  video = element;
}

/** What the attached element reports, for the transport. */
export function reportClipProgress(next: Partial<ClipState>): void {
  if (state.clip === null) return;
  set(next);
}

export function clipState(): ClipState {
  return state;
}

export function toggleClip(): void {
  if (!video) return;
  if (video.paused) void video.play().catch(() => undefined);
  else video.pause();
}

export function playClip(): void {
  void video?.play().catch(() => undefined);
}

export function pauseClip(): void {
  video?.pause();
}

/** Seek the clip, in ms, inside its length. */
export function seekClip(ms: number): void {
  if (!video) return;
  const duration = Number.isFinite(video.duration) ? video.duration : 0;
  const seconds = Math.max(0, ms / 1000);
  video.currentTime = duration > 0 ? Math.min(seconds, duration) : seconds;
  set({ positionMs: Math.round(video.currentTime * 1000) });
}

export function setClipVolume(volume01: number): void {
  if (video) video.volume = Math.min(1, Math.max(0, volume01));
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

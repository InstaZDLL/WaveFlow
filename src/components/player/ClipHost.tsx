import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";

import { usePlayer } from "../../hooks/usePlayer";
import {
  clipState,
  closeClip,
  onClipExit,
  pauseClip,
  playClip,
  requestBackToAudio,
  toggleClip,
  useOpenClip,
  type ClipExit,
} from "../../lib/clipPlayer";
import { playerSetClipOpen, type ClipControl } from "../../lib/tauri/clip";

/** A position within this much of the track's end counts as its end. */
const END_SLACK_MS = 500;

/**
 * Everything a music video (#1073) asks of the track, once for the window:
 * mounted in the layout, it shows nothing itself.
 *
 * - **Switching to the clip** pauses the track, through the same path as
 *   the Play button, so the OS overlay and Discord show it paused. The clip
 *   started at the track's position (`openClipForTrack`).
 * - **Switching back** puts the track at the clip's position, bounded by
 *   the track's length, and plays it again if it was playing. A clip runs
 *   longer than the album version more often than not: past the track's
 *   end, a track that was playing goes on to the next one, as it would have
 *   at its own end, and a paused one waits at its end.
 * - **The end of the clip** is the end of the visit: back to the cover, the
 *   track paused at the clip's position.
 * - **The track starting** on its own — Next, a track picked in the queue,
 *   MPD — means the user went back to the music: the clip gives way, with no
 *   position carried over.
 * - **Where it shows**: the immersive view while it is open, otherwise the
 *   now-playing panel (the Details tab in the Fluid layout). With neither
 *   showing, the clip has nowhere to be and the track takes over again.
 * - **Play and pause from outside the window** reach the clip while it is
 *   open (`player_actions::set_clip_open`, the `clip:control` event).
 */
export function ClipHost() {
  const clip = useOpenClip();
  const {
    isPlaying,
    togglePlayback,
    seek,
    next,
    currentTrack,
    durationMs,
    immersiveOpen,
    activeRightPanel,
  } = usePlayer();

  // The latest values, for handlers that outlive a render.
  const live = useRef({
    isPlaying,
    togglePlayback,
    seek,
    next,
    currentTrackId: currentTrack?.id ?? null,
    durationMs,
  });
  useEffect(() => {
    live.current = {
      isPlaying,
      togglePlayback,
      seek,
      next,
      currentTrackId: currentTrack?.id ?? null,
      durationMs,
    };
  });

  // Every opening is a session of its own (`clipPlayer`).
  const clipKey = clip ? clip.session : null;
  const trackId = clip?.trackId ?? null;
  // Whether the track was playing when the clip opened: when one clip
  // replaces another directly, the track was paused by the first, and what
  // counts is what it was doing before that.
  const resumeTrack = useRef(false);
  const previousKey = useRef<number | null>(null);
  // Whether the clip has seen the track stopped: only a start after that is
  // the user going back to the music, not the pause below racing its own
  // state change.
  const sawStopped = useRef(true);

  // Opening: the track pauses for the clip.
  useEffect(() => {
    const replaced = previousKey.current !== null;
    previousKey.current = clipKey;
    if (clipKey === null) return;
    const { isPlaying: playing, togglePlayback: toggle } = live.current;
    resumeTrack.current = playing || (replaced && resumeTrack.current);
    sawStopped.current = !playing;
    if (playing) void toggle();
  }, [clipKey]);

  useEffect(() => {
    if (clipKey === null) return;
    if (!isPlaying) {
      sawStopped.current = true;
    } else if (sawStopped.current) {
      closeClip();
    }
  }, [isPlaying, clipKey]);

  // Leaving: the track picks up where the clip was.
  useEffect(() => {
    if (trackId === null) return;
    onClipExit((exit: ClipExit, positionMs: number) => {
      closeClip();
      const {
        isPlaying: playing,
        togglePlayback: toggle,
        seek: seekTrack,
        next: nextTrack,
        currentTrackId,
        durationMs: trackMs,
      } = live.current;
      const resume = exit === "user" && resumeTrack.current && !playing;
      if (trackId !== currentTrackId) {
        // A clip opened from the menu for another track: nothing to line
        // up, the track that was playing plays on.
        if (resume) void toggle();
        return;
      }
      void (async () => {
        if (trackMs > 0 && positionMs >= trackMs - END_SLACK_MS) {
          if (resume) {
            await nextTrack();
            return;
          }
          await seekTrack(Math.max(0, trackMs - END_SLACK_MS));
          return;
        }
        await seekTrack(positionMs);
        // A clip opened during the seek is a new visit: resuming now would
        // start the track under it.
        if (resume && clipState().clip === null) await toggle();
      })();
    });
    return () => onClipExit(null);
  }, [trackId]);

  // Where it shows. Read after the render that changed it, so the moment
  // between two surfaces (the panel giving way to the immersive view) does
  // not count as none.
  const hasSurface = immersiveOpen || activeRightPanel === "nowPlaying";
  useEffect(() => {
    if (clipKey !== null && !hasSurface) requestBackToAudio("user");
  }, [clipKey, hasSurface]);

  // Media keys, the tray, the taskbar, the global shortcut and MPD reach
  // the clip while it is open.
  useEffect(() => {
    if (clipKey === null) return;
    void playerSetClipOpen(true).catch((err) =>
      console.error("[ClipHost] set clip open failed", err),
    );
    const unlisten = listen<ClipControl>("clip:control", ({ payload }) => {
      if (payload === "pause") pauseClip();
      else if (payload === "play") playClip();
      // Stop ends the visit; the backend stops the track itself.
      else if (payload === "stop") closeClip();
      else toggleClip();
    });
    return () => {
      void unlisten.then((stop) => stop());
      void playerSetClipOpen(false).catch((err) =>
        console.error("[ClipHost] set clip closed failed", err),
      );
    };
  }, [clipKey]);

  return null;
}

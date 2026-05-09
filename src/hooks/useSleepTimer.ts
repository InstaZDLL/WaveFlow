import { useCallback, useEffect, useRef, useState } from "react";

import { playerPause, playerSetPauseAfterTrack } from "../lib/tauri/player";

/**
 * Sleep-timer state machine.
 *
 * Two activation modes:
 *   - **Duration**: "stop in N minutes" — fires a soft fade-out to
 *     silence, then pauses, then restores the previous volume so the
 *     next session starts at the user's chosen level.
 *   - **End of track**: arms a flag that the caller is expected to
 *     observe on every track-changed event and trigger the same
 *     fade+pause sequence when the current track finishes.
 *
 * Volatile by design — a sleep timer that survives an app restart
 * would be confusing. Refresh the page or close the app and it's
 * gone.
 *
 * The hook is decoupled from `PlayerContext` to avoid pulling state
 * into the player's render tree. It calls the Tauri commands
 * directly and exposes only the minimum surface the UI needs.
 */

/** Total fade-out duration in milliseconds before the final pause. */
const FADE_OUT_MS = 5_000;

/** Number of fade ticks. 50 ticks × 100 ms = 5 s, smooth enough. */
const FADE_TICKS = 50;

export type SleepTimerStatus =
  | { kind: "off" }
  | { kind: "duration"; remainingMs: number; totalMs: number }
  | { kind: "end-of-track" };

interface UseSleepTimerOptions {
  /**
   * Current volume on a 0..100 scale, sourced from `PlayerContext`.
   * The hook reads it once when arming a duration timer so it can
   * restore it after the fade-out completes.
   */
  currentVolume: number;
  /**
   * Setter for the same volume. The hook drives it down to 0 over
   * the fade window then back to the original after the pause so the
   * next playback session starts at the user's last chosen level.
   */
  setVolume: (value: number) => void;
}

interface UseSleepTimerResult {
  status: SleepTimerStatus;
  /** Schedule a duration-based timer in minutes. Replaces any active timer. */
  setDurationMinutes: (minutes: number) => void;
  /** Arm the "stop after current track" mode. */
  setEndOfTrack: () => void;
  /** Cancel any active timer (no fade, immediate). */
  cancel: () => void;
  /** Caller signals that a track just finished — only acts when armed in EoT mode. */
  notifyTrackEnded: () => void;
}

export function useSleepTimer({
  currentVolume,
  setVolume,
}: UseSleepTimerOptions): UseSleepTimerResult {
  const [status, setStatus] = useState<SleepTimerStatus>({ kind: "off" });

  // Refs so cleanup doesn't tear down the running countdown when the
  // caller component re-renders for an unrelated reason.
  const tickHandleRef = useRef<number | null>(null);
  const fadeHandleRef = useRef<number | null>(null);
  const targetTimestampRef = useRef<number | null>(null);
  const restoreVolumeRef = useRef<number>(currentVolume);

  /** Stop every running timer / fade. Idempotent. */
  const clearTimers = useCallback(() => {
    if (tickHandleRef.current != null) {
      window.clearInterval(tickHandleRef.current);
      tickHandleRef.current = null;
    }
    if (fadeHandleRef.current != null) {
      window.clearInterval(fadeHandleRef.current);
      fadeHandleRef.current = null;
    }
    targetTimestampRef.current = null;
  }, []);

  /**
   * Drive the player from its current volume down to silence over
   * FADE_OUT_MS, then issue a pause and restore the original volume
   * so the user's level is preserved for the next session. If the
   * Tauri call to set volume fails we still complete the schedule —
   * the worst case is a slightly louder restart, not a crash.
   */
  const fadeOutAndPause = useCallback(() => {
    const startVolume = restoreVolumeRef.current;
    if (startVolume <= 0) {
      // Already silent — skip the fade and go straight to pause.
      void playerPause().catch((err) =>
        console.error("[SleepTimer] pause failed", err),
      );
      setStatus({ kind: "off" });
      return;
    }

    const stepVolume = startVolume / FADE_TICKS;
    const stepMs = FADE_OUT_MS / FADE_TICKS;
    let ticksDone = 0;

    fadeHandleRef.current = window.setInterval(() => {
      ticksDone += 1;
      if (ticksDone >= FADE_TICKS) {
        if (fadeHandleRef.current != null) {
          window.clearInterval(fadeHandleRef.current);
          fadeHandleRef.current = null;
        }
        // Final pause + restore. Order matters: pause first so the
        // restored volume only applies to the *next* playback, not
        // the silent tail of the current one.
        void playerPause().catch((err) =>
          console.error("[SleepTimer] pause failed", err),
        );
        // Restore through the normal setter so PlayerContext picks
        // up the change and the volume slider snaps back too.
        setVolume(startVolume);
        setStatus({ kind: "off" });
        return;
      }
      const next = Math.max(0, Math.round(startVolume - stepVolume * ticksDone));
      setVolume(next);
    }, stepMs);
  }, [setVolume]);

  const setDurationMinutes = useCallback(
    (minutes: number) => {
      clearTimers();
      // Switching from end-of-track to duration must clear the
      // backend flag so the next natural track end doesn't trigger
      // an unexpected stop.
      void playerSetPauseAfterTrack(false).catch(() => {});
      const totalMs = Math.max(1, Math.round(minutes * 60_000));
      const target = Date.now() + totalMs;
      targetTimestampRef.current = target;
      restoreVolumeRef.current = currentVolume;
      setStatus({ kind: "duration", remainingMs: totalMs, totalMs });

      tickHandleRef.current = window.setInterval(() => {
        const t = targetTimestampRef.current;
        if (t == null) return;
        const remaining = t - Date.now();
        if (remaining <= 0) {
          clearTimers();
          fadeOutAndPause();
          return;
        }
        setStatus({ kind: "duration", remainingMs: remaining, totalMs });
      }, 1_000);
    },
    [clearTimers, currentVolume, fadeOutAndPause],
  );

  const setEndOfTrack = useCallback(() => {
    clearTimers();
    restoreVolumeRef.current = currentVolume;
    // Arm the backend flag so the analytics worker skips its
    // auto-advance step when the current track ends. Without this
    // the next track starts before our track-ended listener can
    // fire pause — the queue cursor advances and the user lands on
    // a new track instead of being paused.
    void playerSetPauseAfterTrack(true).catch((err) =>
      console.error("[SleepTimer] arm pause-after-track failed", err),
    );
    setStatus({ kind: "end-of-track" });
  }, [clearTimers, currentVolume]);

  const cancel = useCallback(() => {
    clearTimers();
    // Always disarm the backend flag on cancel — even if the
    // current mode wasn't end-of-track, that's the safest reset.
    void playerSetPauseAfterTrack(false).catch((err) =>
      console.error("[SleepTimer] disarm pause-after-track failed", err),
    );
    setStatus({ kind: "off" });
  }, [clearTimers]);

  const notifyTrackEnded = useCallback(() => {
    setStatus((current) => {
      if (current.kind !== "end-of-track") return current;
      // Fire the same fade pipeline as the duration path so behaviour
      // is uniform from the user's perspective.
      fadeOutAndPause();
      return { kind: "off" };
    });
  }, [fadeOutAndPause]);

  // Tear down on unmount to avoid leaking timers if the app navigates
  // away while a sleep is armed (shouldn't happen in WaveFlow's
  // single-window model, but cheap insurance).
  useEffect(() => clearTimers, [clearTimers]);

  return {
    status,
    setDurationMinutes,
    setEndOfTrack,
    cancel,
    notifyTrackEnded,
  };
}

import { listen } from "@tauri-apps/api/event";

/**
 * Spectrum frames, held until their audio is played, kept outside the
 * visualizer.
 *
 * The decoder computes a frame up to a ring's length (about a second)
 * before the output plays it, so each `player:spectrum` event says how far
 * ahead it is (`delay_ms`) and waits here until then. The queue used to
 * live in the visualizer itself, which only starts listening when it
 * mounts: opening the immersive view then showed nothing for the length of
 * the ring, the first frame it heard about being a second away. Listening
 * from the start of the window, the frames that are due when the view
 * opens are already here.
 *
 * - **Paused**, the queue holds still, as the ring does, and on resume
 *   the frames queued before the pause wait as long as it lasted. It
 *   follows `player:state` itself, so a pause taken while no visualizer
 *   is open still counts.
 * - **A frame due sooner** than those already waiting means the ring was
 *   emptied by a seek or a load: the later ones describe audio that will
 *   never play, and are dropped.
 *
 * Module state, one per webview, started by `PlayerProvider`.
 */

interface SpectrumPayload {
  bands: number[];
  /** How long until this frame's audio is played. */
  delay_ms: number;
}

interface QueuedFrame {
  due: number;
  /** When it arrived: only frames older than a pause wait it out. */
  receivedAt: number;
  bands: number[];
}

/** A few seconds of frames at most: the ring holds about one. */
const MAX_QUEUED_FRAMES = 120;

const queue: QueuedFrame[] = [];
/** When playback paused, `null` while it is not paused. */
let pausedAt: number | null = null;
let started = false;

function onFrame({ bands, delay_ms }: SpectrumPayload): void {
  const receivedAt = performance.now();
  const due = receivedAt + (delay_ms ?? 0);
  while (queue.length > 0 && queue[queue.length - 1].due > due) queue.pop();
  queue.push({ due, receivedAt, bands });
  if (queue.length > MAX_QUEUED_FRAMES) queue.shift();
}

function onState(state: string): void {
  const now = performance.now();
  if (state === "paused") {
    if (pausedAt === null) pausedAt = now;
    return;
  }
  if (state !== "playing" || pausedAt === null) return;
  const shift = now - pausedAt;
  for (const frame of queue) {
    if (frame.receivedAt < pausedAt) frame.due += shift;
  }
  pausedAt = null;
}

/**
 * Start queueing frames, once per webview. Never stopped: the listeners
 * cost a few array operations per frame, and the backend sends no frames
 * at all while the visualizer setting is off.
 */
export function startSpectrumFeed(): void {
  if (started) return;
  started = true;
  void Promise.allSettled([
    listen<SpectrumPayload>("player:spectrum", (e) => onFrame(e.payload)),
    listen<{ state: string }>("player:state", (e) => onState(e.payload.state)),
  ]).then((results) => {
    const failed = results.find((r) => r.status === "rejected");
    if (!failed) return;
    // Half a feed is worse than none: frames that never pause, or a pause
    // with no frames. Release what did register so the next start does
    // not listen twice.
    for (const r of results) if (r.status === "fulfilled") r.value();
    console.error("[spectrumFeed] listen failed", failed.reason);
    started = false;
  });
}

/**
 * The newest frame whose audio is playing by `now`, taking it and every
 * older one off the queue; `undefined` when none is due, or while paused.
 */
export function takeDueFrame(now: number): number[] | undefined {
  if (pausedAt !== null) return undefined;
  let due: QueuedFrame | undefined;
  while (queue.length > 0 && queue[0].due <= now) due = queue.shift();
  return due?.bands;
}

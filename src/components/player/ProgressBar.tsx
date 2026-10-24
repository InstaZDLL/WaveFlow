import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlayerPosition } from "../../hooks/usePlayerPosition";
import { isRadioTrack } from "../../lib/playerSources";
import { formatDuration } from "../../lib/tauri/track";
import { playerGetAbLoop, type AbLoopSnapshot } from "../../lib/tauri/player";

/* A wave of wavelength 24 units across a 1000-unit box: the Fluid
   skin's progress. The SVG stretches to the bar; the amplitude is pinned
   by the fixed height, so it reads as a line, not a chart. */
const WAVE_PATH = (() => {
  const parts: string[] = ["M0 10"];
  const half = 12;
  for (let x = 0; x < 1000; x += half * 2) {
    parts.push(`q ${half / 2} -4 ${half} 0 t ${half} 0`);
  }
  return parts.join(" ");
})();

interface ProgressBarProps {
  /**
   * `bar`: the classic row (time · rail · time) inside the centre
   * column. `edge`: the Fluid skin's full-width wave along the player
   * bar's top edge, elapsed and total at its ends — same slider, same
   * keyboard contract, three times the travel for a seek. `horizon`:
   * the same place and the same slider, drawn as Lounge's line of
   * light — a straight line whose played part lights the bar below it.
   */
  variant?: "bar" | "edge" | "horizon";
}

/**
 * Interactive progress bar. While the user is dragging, local state
 * owns the thumb position and we ignore incoming `player:position`
 * events (via `setSeeking`); on pointer-up we call `seek(ms)` to
 * commit the target to the backend.
 */
export function ProgressBar({ variant = "bar" }: ProgressBarProps = {}) {
  const { t } = useTranslation();
  const clipId = useId();
  const { durationMs, seek, setSeeking, currentTrack } = usePlayer();
  const positionMs = usePlayerPosition();
  const [dragMs, setDragMs] = useState<number | null>(null);
  const trackRef = useRef<HTMLDivElement | null>(null);

  // A-B loop endpoints — hydrated on mount + kept in sync via the
  // backend's `player:ab-loop` event so the markers render the same
  // values the decoder is enforcing.
  const [abLoop, setAbLoop] = useState<AbLoopSnapshot>({
    a_ms: null,
    b_ms: null,
  });
  useEffect(() => {
    let cancelled = false;
    playerGetAbLoop()
      .then((s) => {
        if (!cancelled) setAbLoop(s);
      })
      .catch(() => {});
    const unlisten = listen<AbLoopSnapshot>("player:ab-loop", (e) => {
      setAbLoop(e.payload);
    });
    return () => {
      cancelled = true;
      unlisten.then((fn) => fn()).catch(() => {});
    };
  }, []);

  const hasTrack = currentTrack != null && durationMs > 0;
  // Anything with a known length is scrubbable: local files, and now remote
  // tracks too (their HTTP source seeks via range requests). Live radio has
  // duration 0, so `hasTrack` already keeps it non-seekable.
  const seekable = hasTrack;
  const displayMs = dragMs ?? positionMs;
  const clampedDisplay = Math.min(
    Math.max(displayMs, 0),
    Math.max(durationMs, 1),
  );
  const percent = hasTrack ? (clampedDisplay / durationMs) * 100 : 0;

  const positionFromPointer = useCallback(
    (clientX: number): number => {
      const el = trackRef.current;
      if (!el || durationMs <= 0) return 0;
      const rect = el.getBoundingClientRect();
      const ratio = Math.min(
        Math.max((clientX - rect.left) / rect.width, 0),
        1,
      );
      return Math.round(ratio * durationMs);
    },
    [durationMs],
  );

  const handlePointerDown = useCallback(
    (e: PointerEvent<HTMLDivElement>) => {
      if (!seekable) return;
      (e.currentTarget as HTMLDivElement).setPointerCapture(e.pointerId);
      setSeeking(true);
      setDragMs(positionFromPointer(e.clientX));
    },
    [seekable, positionFromPointer, setSeeking],
  );

  const handlePointerMove = useCallback(
    (e: PointerEvent<HTMLDivElement>) => {
      if (dragMs == null) return;
      setDragMs(positionFromPointer(e.clientX));
    },
    [dragMs, positionFromPointer],
  );

  const handlePointerUp = useCallback(
    (e: PointerEvent<HTMLDivElement>) => {
      if (dragMs == null) return;
      const target = dragMs;
      setDragMs(null);
      setSeeking(false);
      (e.currentTarget as HTMLDivElement).releasePointerCapture(e.pointerId);
      seek(target);
    },
    [dragMs, seek, setSeeking],
  );

  // Keyboard support for the slider role: arrows nudge by 5 s, Page
  // Up/Down by 30 s, Home/End jump to start/end. Mirrors the browser-
  // standard behaviour of <input type="range"> so screen reader users
  // get the same control as pointer users.
  const STEP_MS = 5_000;
  const PAGE_MS = 30_000;
  const handleKeyDown = useCallback(
    (e: KeyboardEvent<HTMLDivElement>) => {
      if (!seekable) return;
      let next: number;
      switch (e.key) {
        case "ArrowLeft":
        case "ArrowDown":
          next = Math.max(0, positionMs - STEP_MS);
          break;
        case "ArrowRight":
        case "ArrowUp":
          next = Math.min(durationMs, positionMs + STEP_MS);
          break;
        case "PageDown":
          next = Math.max(0, positionMs - PAGE_MS);
          break;
        case "PageUp":
          next = Math.min(durationMs, positionMs + PAGE_MS);
          break;
        case "Home":
          next = 0;
          break;
        case "End":
          next = durationMs;
          break;
        default:
          return;
      }
      e.preventDefault();
      seek(next);
    },
    [seekable, positionMs, durationMs, seek],
  );

  // Live Web Radio has no seekable timeline (the stream started before we
  // tuned in, position is "seconds since I connected"), so the scrubber +
  // timestamps are meaningless — hide the whole row. Placed after all
  // hooks so the Rules of Hooks hold.
  if (isRadioTrack(currentTrack)) return null;

  if (variant === "horizon") {
    // Moved by transforms only (`--fe-progress` is a 0–1 factor), so a
    // position tick recomposites the line and its light, never lays
    // them out again.
    const factor = percent / 100;
    return (
      <div className="fe-horizon relative h-5 w-full px-16">
        <span className="fe-ink-3 fe-mono absolute left-5 top-1/2 -translate-y-1/2 text-[0.75rem]">
          {formatDuration(clampedDisplay)}
        </span>
        <div
          ref={trackRef}
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={handlePointerUp}
          onPointerCancel={handlePointerUp}
          onKeyDown={handleKeyDown}
          role="slider"
          tabIndex={seekable ? 0 : -1}
          aria-label={t("player.seek", "Position")}
          aria-valuemin={0}
          aria-valuemax={hasTrack ? durationMs : 0}
          aria-valuenow={hasTrack ? clampedDisplay : 0}
          aria-valuetext={`${formatDuration(clampedDisplay)} / ${formatDuration(durationMs)}`}
          aria-disabled={!seekable}
          data-seeking={dragMs != null || undefined}
          className={`fe-horizon__track h-full ${seekable ? "cursor-pointer" : "cursor-default"}`}
          style={{ ["--fe-progress" as string]: factor }}
        >
          <span className="fe-horizon__spill" aria-hidden="true" />
          <span className="fe-horizon__line" aria-hidden="true" />
          <span className="fe-horizon__fill" aria-hidden="true" />
          {seekable && (
            // The head is a full-width layer moved by its own width: the
            // rail clips it, or at the end of a track it would hang a
            // window's width past the bar and scroll the document.
            <span className="fe-horizon__rail" aria-hidden="true">
              <span className="fe-horizon__head">
                <span className="fe-horizon__thumb" />
              </span>
            </span>
          )}
          {hasTrack && abLoop.a_ms != null && (
            <AbMarker
              ms={abLoop.a_ms}
              durationMs={durationMs}
              label="A"
              colour="bg-amber-500"
            />
          )}
          {hasTrack && abLoop.b_ms != null && (
            <AbMarker
              ms={abLoop.b_ms}
              durationMs={durationMs}
              label="B"
              colour="bg-rose-500"
            />
          )}
          {hasTrack && abLoop.a_ms != null && abLoop.b_ms != null && (
            <div
              className="absolute top-0 h-full bg-rose-500/25 pointer-events-none"
              style={{
                left: `${(abLoop.a_ms / durationMs) * 100}%`,
                width: `${((abLoop.b_ms - abLoop.a_ms) / durationMs) * 100}%`,
              }}
            />
          )}
        </div>
        <span className="fe-ink-3 fe-mono absolute right-5 top-1/2 -translate-y-1/2 text-[0.75rem]">
          {formatDuration(hasTrack ? durationMs : 0)}
        </span>
      </div>
    );
  }

  if (variant === "edge") {
    return (
      <div className="relative h-5 w-full px-16">
        <span className="fe-ink-3 fe-mono absolute left-5 top-1/2 -translate-y-1/2 text-[0.75rem]">
          {formatDuration(clampedDisplay)}
        </span>
        <div
          ref={trackRef}
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={handlePointerUp}
          onPointerCancel={handlePointerUp}
          onKeyDown={handleKeyDown}
          role="slider"
          tabIndex={seekable ? 0 : -1}
          aria-label={t("player.seek", "Position")}
          aria-valuemin={0}
          aria-valuemax={hasTrack ? durationMs : 0}
          aria-valuenow={hasTrack ? clampedDisplay : 0}
          aria-valuetext={`${formatDuration(clampedDisplay)} / ${formatDuration(durationMs)}`}
          aria-disabled={!seekable}
          className={`fe-progress h-full ${seekable ? "cursor-pointer" : "cursor-default"}`}
        >
          <svg
            viewBox="0 0 1000 20"
            preserveAspectRatio="none"
            aria-hidden="true"
          >
            <defs>
              <clipPath id={clipId}>
                <rect x="0" y="0" width={percent * 10} height="20" />
              </clipPath>
            </defs>
            <line
              x1="0"
              y1="10"
              x2="1000"
              y2="10"
              stroke="rgb(255 255 255 / 0.16)"
              strokeWidth="1.5"
              vectorEffect="non-scaling-stroke"
            />
            <path
              d={WAVE_PATH}
              fill="none"
              stroke="var(--fe-accent)"
              strokeWidth="1.8"
              strokeLinecap="round"
              vectorEffect="non-scaling-stroke"
              clipPath={`url(#${clipId})`}
            />
            {seekable && (
              <circle
                className="fe-progress__thumb"
                cx={percent * 10}
                cy="10"
                r="5"
                fill="var(--fe-ink)"
                vectorEffect="non-scaling-stroke"
                style={{
                  transform: "scale(0.6, 1)",
                  transformOrigin: `${percent * 10}px 10px`,
                }}
              />
            )}
          </svg>
          {hasTrack && abLoop.a_ms != null && (
            <AbMarker
              ms={abLoop.a_ms}
              durationMs={durationMs}
              label="A"
              colour="bg-amber-500"
            />
          )}
          {hasTrack && abLoop.b_ms != null && (
            <AbMarker
              ms={abLoop.b_ms}
              durationMs={durationMs}
              label="B"
              colour="bg-rose-500"
            />
          )}
          {hasTrack && abLoop.a_ms != null && abLoop.b_ms != null && (
            <div
              className="absolute top-0 h-full bg-rose-500/25 pointer-events-none"
              style={{
                left: `${(abLoop.a_ms / durationMs) * 100}%`,
                width: `${((abLoop.b_ms - abLoop.a_ms) / durationMs) * 100}%`,
              }}
            />
          )}
        </div>
        <span className="fe-ink-3 fe-mono absolute right-5 top-1/2 -translate-y-1/2 text-[0.75rem]">
          {formatDuration(hasTrack ? durationMs : 0)}
        </span>
      </div>
    );
  }

  return (
    <div className="w-full flex items-center space-x-3 text-xs text-zinc-400">
      <span className="tabular-nums w-10 text-right">
        {formatDuration(clampedDisplay)}
      </span>
      <div
        ref={trackRef}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        onPointerCancel={handlePointerUp}
        onKeyDown={handleKeyDown}
        role="slider"
        tabIndex={seekable ? 0 : -1}
        aria-label={t("player.seek", "Position")}
        aria-valuemin={0}
        aria-valuemax={hasTrack ? durationMs : 0}
        aria-valuenow={hasTrack ? clampedDisplay : 0}
        aria-valuetext={`${formatDuration(clampedDisplay)} / ${formatDuration(durationMs)}`}
        aria-disabled={!seekable}
        className={`flex-1 h-1.5 rounded-full bg-zinc-200 dark:bg-zinc-700 relative ${
          seekable ? "cursor-pointer" : "cursor-default"
        } group`}
      >
        <div
          className="h-full bg-emerald-500 rounded-full"
          style={{ width: `${percent}%` }}
        />
        {/* A-B loop overlay: tinted region + two coloured marker pins.
            Rendered above the progress fill so the loop is legible
            even on the played portion of the track. */}
        {hasTrack && abLoop.a_ms != null && (
          <AbMarker
            ms={abLoop.a_ms}
            durationMs={durationMs}
            label="A"
            colour="bg-amber-500"
          />
        )}
        {hasTrack && abLoop.b_ms != null && (
          <AbMarker
            ms={abLoop.b_ms}
            durationMs={durationMs}
            label="B"
            colour="bg-rose-500"
          />
        )}
        {hasTrack && abLoop.a_ms != null && abLoop.b_ms != null && (
          <div
            className="absolute top-0 h-full bg-rose-500/15 dark:bg-rose-500/25 pointer-events-none"
            style={{
              left: `${(abLoop.a_ms / durationMs) * 100}%`,
              width: `${((abLoop.b_ms - abLoop.a_ms) / durationMs) * 100}%`,
            }}
          />
        )}
        {seekable && (
          <div
            className="absolute top-1/2 -translate-y-1/2 w-3 h-3 bg-white rounded-full shadow border border-zinc-200 opacity-0 group-hover:opacity-100 transition-opacity"
            style={{ left: `calc(${percent}% - 6px)` }}
          />
        )}
      </div>
      <span className="tabular-nums w-10">
        {formatDuration(hasTrack ? durationMs : 0)}
      </span>
    </div>
  );
}

/**
 * Pin-style marker on the progress track. Vertical bar + a circular
 * label badge sitting just above the bar so the letter (A or B) stays
 * legible even at thin track heights. Click is a no-op — the bar's
 * own pointerdown handles seek; the marker has `pointer-events-none`
 * so it doesn't intercept drags through it.
 */
function AbMarker({
  ms,
  durationMs,
  label,
  colour,
}: {
  ms: number;
  durationMs: number;
  label: string;
  colour: string;
}) {
  const percent = Math.min(Math.max((ms / durationMs) * 100, 0), 100);
  return (
    <div
      className="absolute top-1/2 -translate-y-1/2 pointer-events-none"
      style={{ left: `${percent}%` }}
    >
      <div className={`absolute -translate-x-1/2 w-0.5 h-3 ${colour}`} />
      <div
        className={`absolute -translate-x-1/2 -top-5 ${colour} text-white text-[9px] font-bold px-1 py-px rounded shadow-sm leading-none`}
      >
        {label}
      </div>
    </div>
  );
}

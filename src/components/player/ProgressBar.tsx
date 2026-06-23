import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { usePlayer } from "../../hooks/usePlayer";
import { isRadioTrack } from "../../lib/playerSources";
import { formatDuration } from "../../lib/tauri/track";
import { playerGetAbLoop, type AbLoopSnapshot } from "../../lib/tauri/player";

/**
 * Interactive progress bar. While the user is dragging, local state
 * owns the thumb position and we ignore incoming `player:position`
 * events (via `setSeeking`); on pointer-up we call `seek(ms)` to
 * commit the target to the backend.
 */
export function ProgressBar() {
  const { t } = useTranslation();
  const {
    positionMs,
    durationMs,
    seek,
    setSeeking,
    currentTrack,
    activeProvider,
  } = usePlayer();
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
      if (!hasTrack) return;
      (e.currentTarget as HTMLDivElement).setPointerCapture(e.pointerId);
      setSeeking(true);
      setDragMs(positionFromPointer(e.clientX));
    },
    [hasTrack, positionFromPointer, setSeeking],
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
      if (!hasTrack) return;
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
    [hasTrack, positionMs, durationMs, seek],
  );

  // Live Web Radio has no seekable timeline (the stream started before we
  // tuned in, position is "seconds since I connected"), so the scrubber +
  // timestamps are meaningless — hide the whole row. Placed after all
  // hooks so the Rules of Hooks hold.
  if (isRadioTrack(currentTrack)) return null;

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
        tabIndex={hasTrack ? 0 : -1}
        aria-label={t("player.seek", "Position")}
        aria-valuemin={0}
        aria-valuemax={hasTrack ? durationMs : 0}
        aria-valuenow={hasTrack ? clampedDisplay : 0}
        aria-valuetext={`${formatDuration(clampedDisplay)} / ${formatDuration(durationMs)}`}
        aria-disabled={!hasTrack}
        className={`flex-1 h-1.5 rounded-full bg-zinc-200 dark:bg-zinc-700 relative ${
          hasTrack ? "cursor-pointer" : "cursor-default"
        } group`}
      >
        <div
          className="h-full bg-emerald-500 rounded-full"
          style={{ width: `${percent}%` }}
        />
        {/* A-B loop overlay: tinted region + two coloured marker pins.
            Rendered above the progress fill so the loop is legible
            even on the played portion of the track. */}
        {activeProvider !== "spotify" && hasTrack && abLoop.a_ms != null && (
          <AbMarker
            ms={abLoop.a_ms}
            durationMs={durationMs}
            label="A"
            colour="bg-amber-500"
          />
        )}
        {activeProvider !== "spotify" && hasTrack && abLoop.b_ms != null && (
          <AbMarker
            ms={abLoop.b_ms}
            durationMs={durationMs}
            label="B"
            colour="bg-rose-500"
          />
        )}
        {activeProvider !== "spotify" &&
          hasTrack &&
          abLoop.a_ms != null &&
          abLoop.b_ms != null && (
            <div
              className="absolute top-0 h-full bg-rose-500/15 dark:bg-rose-500/25 pointer-events-none"
              style={{
                left: `${(abLoop.a_ms / durationMs) * 100}%`,
                width: `${((abLoop.b_ms - abLoop.a_ms) / durationMs) * 100}%`,
              }}
            />
          )}
        {hasTrack && (
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

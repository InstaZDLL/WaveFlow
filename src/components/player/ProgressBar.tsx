import { useCallback, useRef, useState, type PointerEvent } from "react";
import { usePlayer } from "../../hooks/usePlayer";
import { formatDuration } from "../../lib/tauri/track";

/**
 * Interactive progress bar. While the user is dragging, local state
 * owns the thumb position and we ignore incoming `player:position`
 * events (via `setSeeking`); on pointer-up we call `seek(ms)` to
 * commit the target to the backend.
 */
export function ProgressBar() {
  const { positionMs, durationMs, seek, setSeeking, currentTrack } = usePlayer();
  const [dragMs, setDragMs] = useState<number | null>(null);
  const trackRef = useRef<HTMLDivElement | null>(null);

  const hasTrack = currentTrack != null && durationMs > 0;
  const displayMs = dragMs ?? positionMs;
  const clampedDisplay = Math.min(Math.max(displayMs, 0), Math.max(durationMs, 1));
  const percent = hasTrack ? (clampedDisplay / durationMs) * 100 : 0;

  const positionFromPointer = useCallback(
    (clientX: number): number => {
      const el = trackRef.current;
      if (!el || durationMs <= 0) return 0;
      const rect = el.getBoundingClientRect();
      const ratio = Math.min(Math.max((clientX - rect.left) / rect.width, 0), 1);
      return Math.round(ratio * durationMs);
    },
    [durationMs]
  );

  const handlePointerDown = useCallback(
    (e: PointerEvent<HTMLDivElement>) => {
      if (!hasTrack) return;
      (e.currentTarget as HTMLDivElement).setPointerCapture(e.pointerId);
      setSeeking(true);
      setDragMs(positionFromPointer(e.clientX));
    },
    [hasTrack, positionFromPointer, setSeeking]
  );

  const handlePointerMove = useCallback(
    (e: PointerEvent<HTMLDivElement>) => {
      if (dragMs == null) return;
      setDragMs(positionFromPointer(e.clientX));
    },
    [dragMs, positionFromPointer]
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
    [dragMs, seek, setSeeking]
  );

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
        className={`flex-1 h-1.5 rounded-full bg-zinc-200 dark:bg-zinc-700 relative ${
          hasTrack ? "cursor-pointer" : "cursor-default"
        } group`}
      >
        <div
          className="h-full bg-emerald-500 rounded-full"
          style={{ width: `${percent}%` }}
        />
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

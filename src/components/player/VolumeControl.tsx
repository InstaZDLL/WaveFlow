import {
  useEffect,
  useRef,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { useTranslation } from "react-i18next";
import { Volume1, Volume2, VolumeX } from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";

/** Scroll-wheel step in percent, matched to the keyboard arrow step
 *  below so wheel + keyboard feel identical. */
const WHEEL_STEP = 5;

export function VolumeControl() {
  const { t } = useTranslation();
  const { volume, setVolume, toggleMute } = usePlayer();
  const trackRef = useRef<HTMLDivElement>(null);
  const wheelHostRef = useRef<HTMLDivElement>(null);

  // Scroll-wheel volume control. React 17+ attaches `wheel` listeners
  // as passive at the root, so an `onWheel={...}` JSX handler can't
  // `preventDefault` to suppress the page scroll behind the player
  // bar. Bind directly with `{ passive: false }` instead.
  useEffect(() => {
    const el = wheelHostRef.current;
    if (!el) return;
    const handler = (e: WheelEvent) => {
      // Ignore horizontal-only scrolls (trackpad swipes deliver
      // `deltaY === 0` with non-zero `deltaX`) — without this guard
      // the `< 0 ? up : down` ternary would treat them as a
      // volume-down tick.
      if (e.deltaY === 0) return;
      e.preventDefault();
      // Negative deltaY = wheel up = volume up. The `setVolume`
      // setter in `PlayerContext` clamps to [0, 100].
      setVolume(volume + (e.deltaY < 0 ? WHEEL_STEP : -WHEEL_STEP));
    };
    el.addEventListener("wheel", handler, { passive: false });
    return () => el.removeEventListener("wheel", handler);
  }, [volume, setVolume]);

  const updateFromClientX = (clientX: number) => {
    const track = trackRef.current;
    if (!track) return;
    const rect = track.getBoundingClientRect();
    if (rect.width === 0) return;
    const ratio = (clientX - rect.left) / rect.width;
    setVolume(ratio * 100);
  };

  const handlePointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    // Stop the browser from interpreting the gesture as a text /
    // image drag — when that happens the pointer-event stream gets
    // hijacked, the cursor flips to "no-drop", and the slider stops
    // tracking the mouse. `preventDefault` on pointerdown reliably
    // suppresses that fallback path inside Tauri's WebView.
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    updateFromClientX(e.clientX);
  };

  const handlePointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!e.currentTarget.hasPointerCapture(e.pointerId)) return;
    updateFromClientX(e.clientX);
  };

  const handlePointerUp = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
  };

  const handleKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    switch (e.key) {
      case "ArrowLeft":
      case "ArrowDown":
        e.preventDefault();
        setVolume(volume - 5);
        break;
      case "ArrowRight":
      case "ArrowUp":
        e.preventDefault();
        setVolume(volume + 5);
        break;
      case "Home":
        e.preventDefault();
        setVolume(0);
        break;
      case "End":
        e.preventDefault();
        setVolume(100);
        break;
    }
  };

  const Icon = volume === 0 ? VolumeX : volume < 50 ? Volume1 : Volume2;

  return (
    <>
      <div ref={wheelHostRef} className="flex items-center space-x-2 w-32">
        <button
          type="button"
          onClick={toggleMute}
          aria-label={
            volume === 0 ? t("player.volume.unmute") : t("player.volume.mute")
          }
          className="text-zinc-400 hover:text-zinc-800 dark:hover:text-white transition-colors"
        >
          <Icon size={20} />
        </button>
        <div
          ref={trackRef}
          role="slider"
          tabIndex={0}
          aria-label={t("player.volume.label")}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={volume}
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={handlePointerUp}
          onPointerCancel={handlePointerUp}
          onDragStart={(e) => e.preventDefault()}
          onKeyDown={handleKeyDown}
          className="flex-1 flex items-center h-6 cursor-pointer touch-none select-none group focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 rounded-full"
        >
          <div className="relative w-full h-1.5 rounded-full bg-zinc-200 dark:bg-zinc-700">
            <div
              className="h-full bg-emerald-500 rounded-full"
              style={{ width: `${volume}%` }}
            />
            <div
              className="absolute top-1/2 w-3 h-3 bg-white rounded-full shadow border border-zinc-200 -translate-y-1/2 -translate-x-1/2 opacity-0 group-hover:opacity-100 transition-opacity"
              style={{ left: `${volume}%` }}
            />
          </div>
        </div>
      </div>
      <span className="text-xs text-zinc-400 tabular-nums w-10 text-right">
        {volume}%
      </span>
    </>
  );
}

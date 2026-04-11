import {
  useRef,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { useTranslation } from "react-i18next";
import { Volume1, Volume2, VolumeX } from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";

export function VolumeControl() {
  const { t } = useTranslation();
  const { volume, setVolume, toggleMute } = usePlayer();
  const trackRef = useRef<HTMLDivElement>(null);

  const updateFromClientX = (clientX: number) => {
    const track = trackRef.current;
    if (!track) return;
    const rect = track.getBoundingClientRect();
    if (rect.width === 0) return;
    const ratio = (clientX - rect.left) / rect.width;
    setVolume(ratio * 100);
  };

  const handlePointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    updateFromClientX(e.clientX);
  };

  const handlePointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!e.currentTarget.hasPointerCapture(e.pointerId)) return;
    updateFromClientX(e.clientX);
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
      <div className="flex items-center space-x-2 w-32">
        <button
          type="button"
          onClick={toggleMute}
          aria-label={volume === 0 ? t("player.volume.unmute") : t("player.volume.mute")}
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
          onKeyDown={handleKeyDown}
          className="flex-1 flex items-center h-6 cursor-pointer touch-none group focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 rounded-full"
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

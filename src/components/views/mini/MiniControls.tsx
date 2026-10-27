import { useTranslation } from "react-i18next";
import {
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  useEffect,
  useRef,
} from "react";
import {
  Pause,
  Play,
  Repeat,
  Repeat1,
  Shuffle,
  SkipBack,
  SkipForward,
  Volume1,
  Volume2,
  VolumeX,
} from "lucide-react";

interface CoverWithControlsProps {
  showControls: boolean;
  onMouseEnter: () => void;
  onMouseLeave: () => void;
  isPlaying: boolean;
  repeatMode: "off" | "all" | "one";
  isShuffled: boolean;
  onPlayPause: () => void;
  onPrev: () => void;
  onNext: () => void;
  onCycleRepeat: () => void;
  onToggleShuffle: () => void;
  /** Remote-queue tracks have no shuffle (matches PlaybackControls). */
  shuffleDisabled: boolean;
  volume: number;
  onSetVolume: (value: number) => void;
  onToggleMute: () => void;
  artworkSlot: React.ReactNode;
}

export function CoverWithControls({
  showControls,
  onMouseEnter,
  onMouseLeave,
  isPlaying,
  repeatMode,
  isShuffled,
  onPlayPause,
  onPrev,
  onNext,
  onCycleRepeat,
  onToggleShuffle,
  shuffleDisabled,
  volume,
  onSetVolume,
  onToggleMute,
  artworkSlot,
}: CoverWithControlsProps) {
  const { t } = useTranslation();
  const ref = useRef<HTMLDivElement | null>(null);
  return (
    <div
      ref={ref}
      onMouseEnter={onMouseEnter}
      onMouseLeave={onMouseLeave}
      // The overlay is hover-revealed, but its controls stay in the tab
      // order — a keyboard user would otherwise be operating buttons and
      // a volume slider they can't see. Focus reveals it too, and a focus
      // leaving the subtree entirely hides it again.
      onFocusCapture={onMouseEnter}
      onBlurCapture={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget)) onMouseLeave();
      }}
      className="relative aspect-square w-full max-w-64 rounded-xl shadow-2xl overflow-hidden"
    >
      {artworkSlot}
      {/* Dimming layer + control bar fade in on hover. */}
      <div
        className={`absolute inset-0 flex flex-col items-center justify-center gap-3 transition-opacity duration-150 ${
          showControls ? "opacity-100 bg-black/40" : "opacity-0"
        }`}
      >
        <div className="flex items-center gap-2">
          <IconButton
            onClick={onToggleShuffle}
            label={t(
              isShuffled
                ? "player.controls.shuffleTracks"
                : "player.controls.shuffleOff",
            )}
            active={isShuffled}
            disabled={shuffleDisabled}
          >
            <Shuffle size={14} />
          </IconButton>
          <IconButton onClick={onPrev} label={t("player.controls.previous")}>
            <SkipBack size={16} />
          </IconButton>
          <button
            type="button"
            onClick={onPlayPause}
            aria-label={
              isPlaying ? t("player.controls.pause") : t("player.controls.play")
            }
            className="w-11 h-11 rounded-full bg-white text-black flex items-center justify-center hover:scale-105 transition-transform"
          >
            {isPlaying ? (
              <Pause size={18} className="fill-current" />
            ) : (
              <Play size={18} className="fill-current ml-0.5" />
            )}
          </button>
          <IconButton onClick={onNext} label={t("player.controls.next")}>
            <SkipForward size={16} />
          </IconButton>
          <IconButton
            onClick={onCycleRepeat}
            label={t(
              repeatMode === "one"
                ? "player.controls.repeatOne"
                : repeatMode === "all"
                  ? "player.controls.repeatAll"
                  : "player.controls.repeatOff",
            )}
            active={repeatMode !== "off"}
          >
            {repeatMode === "one" ? (
              <Repeat1 size={14} />
            ) : (
              <Repeat size={14} />
            )}
          </IconButton>
        </div>
        <MiniVolume
          volume={volume}
          onSetVolume={onSetVolume}
          onToggleMute={onToggleMute}
        />
      </div>
    </div>
  );
}

/** Volume step for the wheel and the arrow keys, matching the
 *  PlayerBar's [`VolumeControl`](../player/VolumeControl.tsx). */
export const VOLUME_STEP = 5;

/**
 * Compact volume slider + mute for the cover overlay (#511).
 *
 * Same interaction contract as the PlayerBar control — pointer drag,
 * wheel, arrows / Home / End — restyled for the widget's translucent
 * palette, and sitting under the transport row so the idle state stays
 * "just the artwork". Volume itself is engine-wide: the backend echoes
 * every change on `player:volume-changed`, so this slider and the main
 * window's stay in step.
 */
function MiniVolume({
  volume,
  onSetVolume,
  onToggleMute,
}: {
  volume: number;
  onSetVolume: (value: number) => void;
  onToggleMute: () => void;
}) {
  const { t } = useTranslation();
  const trackRef = useRef<HTMLDivElement | null>(null);
  const hostRef = useRef<HTMLDivElement | null>(null);

  // React attaches `wheel` passively at the root, so a JSX `onWheel`
  // can't `preventDefault`. Bind directly to keep the gesture from
  // scrolling anything behind the widget.
  useEffect(() => {
    const el = hostRef.current;
    if (!el) return;
    const handler = (e: WheelEvent) => {
      if (e.deltaY === 0) return;
      e.preventDefault();
      onSetVolume(volume + (e.deltaY < 0 ? VOLUME_STEP : -VOLUME_STEP));
    };
    el.addEventListener("wheel", handler, { passive: false });
    return () => el.removeEventListener("wheel", handler);
  }, [volume, onSetVolume]);

  const updateFromClientX = (clientX: number) => {
    const el = trackRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    if (rect.width === 0) return;
    onSetVolume(((clientX - rect.left) / rect.width) * 100);
  };

  const handlePointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    // Suppress the WebView's image/text drag fallback, which otherwise
    // hijacks the pointer stream mid-drag (same reason as VolumeControl).
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
        onSetVolume(volume - VOLUME_STEP);
        break;
      case "ArrowRight":
      case "ArrowUp":
        e.preventDefault();
        onSetVolume(volume + VOLUME_STEP);
        break;
      case "Home":
        e.preventDefault();
        onSetVolume(0);
        break;
      case "End":
        e.preventDefault();
        onSetVolume(100);
        break;
    }
  };

  const Icon = volume === 0 ? VolumeX : volume < 50 ? Volume1 : Volume2;

  return (
    <div ref={hostRef} className="flex items-center gap-2 w-2/3 max-w-40">
      <button
        type="button"
        onClick={onToggleMute}
        aria-label={
          volume === 0 ? t("player.volume.unmute") : t("player.volume.mute")
        }
        title={
          volume === 0 ? t("player.volume.unmute") : t("player.volume.mute")
        }
        className="p-1 -m-1 shrink-0 rounded-full text-white/80 hover:text-white transition-colors"
      >
        <Icon size={14} />
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
        className="group flex-1 flex items-center h-5 cursor-pointer touch-none select-none rounded-full focus:outline-none focus-visible:ring-2 focus-visible:ring-white/70"
      >
        <div className="relative w-full h-1 rounded-full bg-white/25">
          <div
            className="h-full rounded-full bg-white"
            style={{ width: `${volume}%` }}
          />
          <div
            className="absolute top-1/2 w-2.5 h-2.5 rounded-full bg-white shadow -translate-y-1/2 -translate-x-1/2 opacity-0 reveal:opacity-100 transition-opacity"
            style={{ left: `${volume}%` }}
          />
        </div>
      </div>
    </div>
  );
}

export function IconButton({
  onClick,
  label,
  active,
  disabled,
  children,
}: {
  onClick: () => void;
  label: string;
  active?: boolean;
  disabled?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      aria-label={label}
      className={`p-2 rounded-full transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${
        active
          ? "text-emerald-400 hover:bg-white/10"
          : "text-white/80 hover:text-white hover:bg-white/10"
      }`}
    >
      {children}
    </button>
  );
}

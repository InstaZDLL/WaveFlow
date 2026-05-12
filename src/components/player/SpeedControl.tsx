import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { usePlayer } from "../../hooks/usePlayer";

const PRESETS = [0.75, 1.0, 1.25, 1.5, 2.0];
const MIN = 0.5;
const MAX = 2.0;

function formatSpeed(value: number): string {
  // Drop the trailing zero on integer multipliers so "1×" doesn't
  // read as "1.0×" alongside "1.25×". The × sign is kept literal
  // because it's recognisable across every locale (Pocket Casts,
  // YouTube and Audible all use it as-is, not translated).
  const trimmed = Number.isInteger(value) ? value.toFixed(0) : value.toFixed(2);
  // Strip a trailing zero on .X0 values ("1.50" → "1.5") for
  // visual compactness — the pill is meant to stay narrow.
  return `${trimmed.replace(/(\.\d)0$/, "$1")}×`;
}

/**
 * Compact text pill that opens a popover with playback-speed
 * controls. Trigger is a plain text button (no icon) so it stays as
 * small as the volume's "80%" label and slots next to it in the
 * crowded player bar.
 */
export function SpeedControl() {
  const { t } = useTranslation();
  const { playbackSpeed, setPlaybackSpeed } = usePlayer();
  const [isOpen, setIsOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!isOpen) return;
    const onPointer = (event: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(event.target as Node)
      ) {
        setIsOpen(false);
      }
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setIsOpen(false);
    };
    document.addEventListener("mousedown", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [isOpen]);

  const isCustomSpeed = !PRESETS.includes(playbackSpeed);
  const isOffPreset = playbackSpeed !== 1.0;

  return (
    <div ref={containerRef} className="relative">
      <button
        type="button"
        onClick={() => setIsOpen((open) => !open)}
        aria-label={t("player.speed.label", { value: formatSpeed(playbackSpeed) })}
        aria-haspopup="dialog"
        aria-expanded={isOpen}
        title={t("player.speed.title")}
        className={`px-2 h-7 text-xs font-semibold tabular-nums rounded-full transition-colors border ${
          isOffPreset
            ? "border-emerald-500 text-emerald-600 dark:text-emerald-400 bg-emerald-500/10"
            : "border-transparent text-zinc-500 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800"
        }`}
      >
        {formatSpeed(playbackSpeed)}
      </button>

      {isOpen && (
        <div
          role="dialog"
          aria-label={t("player.speed.title")}
          className="absolute bottom-full right-0 mb-3 w-64 p-4 rounded-xl bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 shadow-xl z-50"
        >
          <div className="flex items-center justify-between mb-3">
            <span className="text-xs font-semibold text-zinc-700 dark:text-zinc-200">
              {t("player.speed.title")}
            </span>
            <span className="text-sm font-bold tabular-nums text-emerald-600 dark:text-emerald-400">
              {formatSpeed(playbackSpeed)}
            </span>
          </div>

          <input
            type="range"
            min={MIN}
            max={MAX}
            step={0.05}
            value={playbackSpeed}
            onChange={(e) => setPlaybackSpeed(parseFloat(e.target.value))}
            aria-label={t("player.speed.slider")}
            className="w-full accent-emerald-500"
          />

          <div className="mt-3 grid grid-cols-5 gap-1">
            {PRESETS.map((preset) => {
              const active = Math.abs(playbackSpeed - preset) < 0.001;
              return (
                <button
                  key={preset}
                  type="button"
                  onClick={() => setPlaybackSpeed(preset)}
                  className={`py-1 text-[11px] font-semibold tabular-nums rounded-md transition-colors ${
                    active
                      ? "bg-emerald-500 text-white"
                      : "bg-zinc-100 dark:bg-zinc-800 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-200 dark:hover:bg-zinc-700"
                  }`}
                >
                  {formatSpeed(preset)}
                </button>
              );
            })}
          </div>

          {isCustomSpeed && (
            <p className="mt-3 text-[10px] text-zinc-500 dark:text-zinc-400 italic">
              {t("player.speed.custom")}
            </p>
          )}
          <p className="mt-2 text-[10px] text-zinc-400 dark:text-zinc-500 leading-snug">
            {t("player.speed.pitchHint")}
          </p>
        </div>
      )}
    </div>
  );
}

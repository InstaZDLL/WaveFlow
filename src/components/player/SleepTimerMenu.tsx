import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AnimatePresence, motion } from "framer-motion";
import { Moon, X } from "lucide-react";

import type { SleepTimerStatus } from "../../hooks/useSleepTimer";

/// Preset durations exposed in the popover. The list mirrors what
/// Spotify, Apple Music, YouTube Music and most podcast apps offer.
const PRESETS_MIN = [5, 15, 30, 45, 60, 90];

interface SleepTimerMenuProps {
  status: SleepTimerStatus;
  onSetDuration: (minutes: number) => void;
  onSetEndOfTrack: () => void;
  onCancel: () => void;
}

/**
 * Anchored popover with sleep-timer presets + end-of-track + a
 * custom-minutes input. The trigger button lives in the player bar
 * and shows the live countdown when an active timer is running so
 * the user doesn't have to open the popover to read it.
 */
export function SleepTimerMenu({
  status,
  onSetDuration,
  onSetEndOfTrack,
  onCancel,
}: SleepTimerMenuProps) {
  const { t } = useTranslation();
  const [isOpen, setIsOpen] = useState(false);
  const [customMinutes, setCustomMinutes] = useState("");
  const containerRef = useRef<HTMLDivElement>(null);

  // Close on outside click + Escape so the popover stays out of the
  // way without an explicit close button at the top.
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

  const isArmed = status.kind !== "off";
  const triggerLabel =
    status.kind === "duration"
      ? formatRemaining(status.remainingMs)
      : status.kind === "end-of-track"
        ? t("sleepTimer.endOfTrackBadge")
        : null;

  const handlePreset = (minutes: number) => {
    onSetDuration(minutes);
    setIsOpen(false);
  };

  const handleEndOfTrack = () => {
    onSetEndOfTrack();
    setIsOpen(false);
  };

  const handleCustom = (event: React.FormEvent) => {
    event.preventDefault();
    const minutes = parseInt(customMinutes, 10);
    if (Number.isFinite(minutes) && minutes > 0 && minutes <= 720) {
      onSetDuration(minutes);
      setCustomMinutes("");
      setIsOpen(false);
    }
  };

  const handleCancel = () => {
    onCancel();
    setIsOpen(false);
  };

  return (
    <div ref={containerRef} className="relative">
      <button
        type="button"
        onClick={() => setIsOpen((v) => !v)}
        aria-label={t("sleepTimer.title")}
        title={t("sleepTimer.title")}
        className={`relative p-2 rounded-lg transition-colors ${
          isArmed
            ? "text-emerald-500 hover:text-emerald-400"
            : "text-zinc-500 hover:text-zinc-700 dark:text-zinc-400 dark:hover:text-zinc-200"
        }`}
      >
        <Moon size={18} />
        {triggerLabel && (
          <span className="absolute -top-1 -right-1 px-1 min-w-[18px] h-[16px] flex items-center justify-center rounded-full bg-emerald-500 text-white text-[9px] font-bold leading-none">
            {triggerLabel}
          </span>
        )}
      </button>

      <AnimatePresence>
        {isOpen && (
          <motion.div
            role="menu"
            initial={{ opacity: 0, scale: 0.96, y: 6 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.97, y: 4 }}
            transition={{
              type: "spring",
              stiffness: 480,
              damping: 30,
              mass: 0.5,
            }}
            style={{ transformOrigin: "bottom right" }}
            className="absolute bottom-full right-0 mb-2 w-64 p-3 rounded-2xl shadow-xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-900 z-50 space-y-3"
          >
            <div className="flex items-center justify-between">
              <h3 className="text-xs font-bold uppercase tracking-widest text-zinc-400">
                {t("sleepTimer.title")}
              </h3>
              {isArmed && (
                <button
                  type="button"
                  onClick={handleCancel}
                  className="flex items-center space-x-1 text-xs text-rose-500 hover:text-rose-400"
                >
                  <X size={12} />
                  <span>{t("sleepTimer.cancel")}</span>
                </button>
              )}
            </div>

            {/* Presets grid */}
            <div className="grid grid-cols-3 gap-2">
              {PRESETS_MIN.map((m) => (
                <button
                  key={m}
                  type="button"
                  onClick={() => handlePreset(m)}
                  className="px-3 py-2 rounded-xl text-sm font-medium text-zinc-700 dark:text-zinc-200 bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 transition-colors"
                >
                  {t("sleepTimer.minutes", { count: m })}
                </button>
              ))}
            </div>

            {/* End of track */}
            <button
              type="button"
              onClick={handleEndOfTrack}
              className="w-full px-3 py-2 rounded-xl text-sm font-medium text-zinc-700 dark:text-zinc-200 bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 transition-colors"
            >
              {t("sleepTimer.endOfTrack")}
            </button>

            {/* Custom */}
            <form onSubmit={handleCustom} className="flex space-x-2">
              <input
                type="number"
                min={1}
                max={720}
                value={customMinutes}
                onChange={(e) => setCustomMinutes(e.target.value)}
                placeholder={t("sleepTimer.customPlaceholder")}
                className="flex-1 px-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500"
              />
              <button
                type="submit"
                className="px-4 py-2 rounded-xl text-sm font-medium bg-emerald-500 text-white hover:bg-emerald-600 transition-colors disabled:opacity-50"
                disabled={!customMinutes}
              >
                {t("sleepTimer.start")}
              </button>
            </form>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

/**
 * Format the remaining ms as a compact string suitable for the
 * trigger-button badge. ≥1 hour shows `Hh`, otherwise minutes (`Nm`)
 * and finally seconds (`Ns`) for the final minute so users can see
 * the timer winding down.
 */
function formatRemaining(ms: number): string {
  const totalSec = Math.max(0, Math.ceil(ms / 1000));
  if (totalSec >= 3600) {
    const h = Math.ceil(totalSec / 3600);
    return `${h}h`;
  }
  if (totalSec >= 60) {
    const m = Math.ceil(totalSec / 60);
    return `${m}m`;
  }
  return `${totalSec}s`;
}

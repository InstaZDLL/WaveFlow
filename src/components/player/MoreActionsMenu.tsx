import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AnimatePresence, motion } from "framer-motion";
import { MoreHorizontal, Moon, SlidersHorizontal, X } from "lucide-react";

import type { SleepTimerStatus } from "../../hooks/useSleepTimer";
import { usePlayer } from "../../hooks/usePlayer";
import { AbLoopButton } from "./AbLoopButton";
import { EqPresetPanel } from "./EqPresetButton";

const SLEEP_PRESETS_MIN = [5, 15, 30, 45, 60, 90];
const SPEED_PRESETS = [0.75, 1.0, 1.25, 1.5, 2.0];
const SPEED_MIN = 0.5;
const SPEED_MAX = 2.0;

interface MoreActionsMenuProps {
  /** When `true`, A-B loop is pinned as a primary button in the bar
   *  and the overflow menu doesn't duplicate it. */
  pinAbLoop: boolean;
  /** When `true`, sleep timer is pinned as a primary button in the
   *  bar and the overflow menu doesn't duplicate it. */
  pinSleepTimer: boolean;
  /** When `true`, the EQ preset popover is pinned as a primary
   *  button in the bar and the overflow menu doesn't duplicate it. */
  pinEqPreset: boolean;
  /** When `true`, render the playback-speed section inside the menu.
   *  Hidden in Spotify mode (Web Playback SDK has no speed control). */
  showSpeed: boolean;
  /** When `true`, render the EQ preset list inside the menu (when
   *  not pinned). Hidden in Spotify mode for the same reason as
   *  speed — Web Playback SDK doesn't run through our audio engine. */
  showEq: boolean;
  sleepTimer: {
    status: SleepTimerStatus;
    onSetDuration: (minutes: number) => void;
    onSetEndOfTrack: () => void;
    onCancel: () => void;
  };
}

/**
 * Overflow popover for the player bar's secondary actions. Hosts
 * Sleep timer, A-B loop and playback speed. Sleep timer / A-B loop
 * can be pinned to the bar via Settings; speed has no pin (used too
 * rarely to deserve a permanent slot). The caller is expected to
 * skip rendering this component entirely when nothing would go
 * inside (both pinned + Spotify mode hides speed too).
 */
export function MoreActionsMenu({
  pinAbLoop,
  pinSleepTimer,
  pinEqPreset,
  showSpeed,
  showEq,
  sleepTimer,
}: MoreActionsMenuProps) {
  const { t } = useTranslation();
  const { playbackSpeed, setPlaybackSpeed } = usePlayer();
  const [isOpen, setIsOpen] = useState(false);
  const [customMinutes, setCustomMinutes] = useState("");
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

  const showSleepInMenu = !pinSleepTimer;
  const showAbInMenu = !pinAbLoop;
  const showEqInMenu = showEq && !pinEqPreset;

  const sleepArmed = sleepTimer.status.kind !== "off";
  const sleepBadge =
    sleepTimer.status.kind === "duration"
      ? formatRemaining(sleepTimer.status.remainingMs)
      : sleepTimer.status.kind === "end-of-track"
        ? t("sleepTimer.endOfTrackBadge")
        : null;

  // Trigger badge priority: sleep-timer countdown > non-default speed.
  // Both are mutually exclusive in the same corner so the user always
  // sees the most time-sensitive signal first. `isOffSpeed` is gated
  // on `showSpeed` because the speed UI is hidden in Spotify mode —
  // tinting the trigger green for a value the user can't even see
  // from this menu would be misleading.
  const isOffSpeed = showSpeed && Math.abs(playbackSpeed - 1.0) > 0.001;
  const speedBadge = isOffSpeed ? formatSpeed(playbackSpeed) : null;
  const triggerBadge = sleepBadge && showSleepInMenu ? sleepBadge : speedBadge;
  const triggerBadgeTone =
    sleepBadge && showSleepInMenu
      ? "bg-emerald-500 text-white"
      : "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400 border border-emerald-500/40";

  const handleCustomSleep = (event: React.FormEvent) => {
    event.preventDefault();
    const minutes = parseInt(customMinutes, 10);
    if (Number.isFinite(minutes) && minutes > 0 && minutes <= 720) {
      sleepTimer.onSetDuration(minutes);
      setCustomMinutes("");
      setIsOpen(false);
    }
  };

  return (
    <div ref={containerRef} className="relative">
      <button
        type="button"
        onClick={() => setIsOpen((open) => !open)}
        aria-label={t("playerBar.moreActions")}
        aria-haspopup="dialog"
        aria-expanded={isOpen}
        title={t("playerBar.moreActions")}
        className={`relative p-2 rounded-lg transition-colors ${
          isOpen
            ? "text-emerald-500"
            : (sleepArmed && showSleepInMenu) || isOffSpeed
              ? "text-emerald-500 hover:text-emerald-400"
              : "text-zinc-400 hover:text-zinc-800 dark:hover:text-white"
        }`}
      >
        <MoreHorizontal size={20} />
        {triggerBadge && (
          <span
            className={`absolute -top-1 -right-1 px-1 min-w-[18px] h-[16px] flex items-center justify-center rounded-full text-[9px] font-bold leading-none ${triggerBadgeTone}`}
          >
            {triggerBadge}
          </span>
        )}
      </button>

      <AnimatePresence>
        {isOpen && (
          <motion.div
            role="dialog"
            aria-label={t("playerBar.moreActions")}
            initial={{ opacity: 0, scale: 0.96, y: 6 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.97, y: 4 }}
            transition={{ type: "spring", stiffness: 480, damping: 30, mass: 0.5 }}
            style={{ transformOrigin: "bottom right" }}
            className="absolute bottom-full right-0 mb-3 w-72 max-h-[calc(100dvh-7rem)] overflow-y-auto overscroll-contain p-1 rounded-xl bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 shadow-xl z-50"
          >
          {showSpeed && (
            <div className="px-3 py-2 space-y-2">
              <div className="flex items-center justify-between">
                <div className="text-xs font-bold uppercase tracking-widest text-zinc-400">
                  {t("player.speed.title")}
                </div>
                <span className="text-sm font-bold tabular-nums text-emerald-600 dark:text-emerald-400">
                  {formatSpeed(playbackSpeed)}
                </span>
              </div>

              <input
                type="range"
                min={SPEED_MIN}
                max={SPEED_MAX}
                step={0.05}
                value={playbackSpeed}
                onChange={(e) => setPlaybackSpeed(parseFloat(e.target.value))}
                aria-label={t("player.speed.slider")}
                className="w-full accent-emerald-500"
              />

              <div className="grid grid-cols-5 gap-1">
                {SPEED_PRESETS.map((preset) => {
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
            </div>
          )}

          {showSpeed &&
            (showEqInMenu || showAbInMenu || showSleepInMenu) && (
              <div className="my-1 h-px bg-zinc-100 dark:bg-zinc-800" />
            )}

          {/* EQ preset list — same panel as the primary-pin popover,
              rendered inline here when the user hasn't pinned the EQ
              button. Bypass toggle + 20 built-in presets. The full
              draggable curve still lives in Settings → Lecture. */}
          {showEqInMenu && (
            <div className="px-3 py-2 space-y-2">
              <div className="flex items-center gap-2 text-xs font-bold uppercase tracking-widest text-zinc-400">
                <SlidersHorizontal size={14} />
                {t("playerBar.eqPreset")}
              </div>
              <div className="rounded-lg border border-zinc-100 dark:border-zinc-800 overflow-hidden">
                <EqPresetPanel onPick={() => setIsOpen(false)} />
              </div>
            </div>
          )}

          {showEqInMenu && (showAbInMenu || showSleepInMenu) && (
            <div className="my-1 h-px bg-zinc-100 dark:bg-zinc-800" />
          )}

          {showAbInMenu && (
            <div className="flex items-center justify-between gap-2 px-3 py-2 rounded-lg hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors">
              <span className="text-sm text-zinc-700 dark:text-zinc-200">
                {t("playerBar.abLoop")}
              </span>
              <AbLoopButton />
            </div>
          )}

          {showSleepInMenu && showAbInMenu && (
            <div className="my-1 h-px bg-zinc-100 dark:bg-zinc-800" />
          )}

          {showSleepInMenu && (
            <div className="px-3 py-2 space-y-2">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2 text-xs font-bold uppercase tracking-widest text-zinc-400">
                  <Moon size={14} />
                  {t("sleepTimer.title")}
                </div>
                {sleepArmed && (
                  <button
                    type="button"
                    onClick={() => {
                      sleepTimer.onCancel();
                      setIsOpen(false);
                    }}
                    className="flex items-center space-x-1 text-xs text-rose-500 hover:text-rose-400"
                  >
                    <X size={12} />
                    <span>{t("sleepTimer.cancel")}</span>
                  </button>
                )}
              </div>

              <div className="grid grid-cols-3 gap-2">
                {SLEEP_PRESETS_MIN.map((m) => (
                  <button
                    key={m}
                    type="button"
                    onClick={() => {
                      sleepTimer.onSetDuration(m);
                      setIsOpen(false);
                    }}
                    className="px-2 py-1.5 rounded-lg text-xs font-medium text-zinc-700 dark:text-zinc-200 bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 transition-colors"
                  >
                    {t("sleepTimer.minutes", { count: m })}
                  </button>
                ))}
              </div>

              <button
                type="button"
                onClick={() => {
                  sleepTimer.onSetEndOfTrack();
                  setIsOpen(false);
                }}
                className="w-full px-3 py-1.5 rounded-lg text-xs font-medium text-zinc-700 dark:text-zinc-200 bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 transition-colors"
              >
                {t("sleepTimer.endOfTrack")}
              </button>

              <form onSubmit={handleCustomSleep} className="flex gap-2">
                <input
                  type="number"
                  min={1}
                  max={720}
                  value={customMinutes}
                  onChange={(e) => setCustomMinutes(e.target.value)}
                  placeholder={t("sleepTimer.customPlaceholder")}
                  aria-label={t("sleepTimer.customAriaLabel")}
                  className="flex-1 px-2 py-1.5 rounded-lg text-xs bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500"
                />
                <button
                  type="submit"
                  className="px-3 py-1.5 rounded-lg text-xs font-medium bg-emerald-500 text-white hover:bg-emerald-600 transition-colors disabled:opacity-50"
                  disabled={!customMinutes}
                >
                  {t("sleepTimer.start")}
                </button>
              </form>
            </div>
          )}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

function formatRemaining(ms: number): string {
  // totalSec uses ceil so the final second of countdown still reads
  // "1s" rather than "0s". h/m branches use floor so "1h 1m" shows
  // as "1h" instead of misleadingly rounding up to "2h".
  const totalSec = Math.max(0, Math.ceil(ms / 1000));
  if (totalSec >= 3600) {
    const h = Math.floor(totalSec / 3600);
    return `${h}h`;
  }
  if (totalSec >= 60) {
    const m = Math.floor(totalSec / 60);
    return `${m}m`;
  }
  return `${totalSec}s`;
}

function formatSpeed(value: number): string {
  const trimmed = Number.isInteger(value) ? value.toFixed(0) : value.toFixed(2);
  return `${trimmed.replace(/(\.\d)0$/, "$1")}×`;
}

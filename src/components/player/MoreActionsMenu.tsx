import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Maximize2,
  MoreHorizontal,
  PictureInPicture2,
  Moon,
  X,
} from "lucide-react";

import type { SleepTimerStatus } from "../../hooks/useSleepTimer";
import { AbLoopButton } from "./AbLoopButton";

const SLEEP_PRESETS_MIN = [5, 15, 30, 45, 60, 90];

interface MoreActionsMenuProps {
  /** When `false`, the mini-player entry is hidden — used in Spotify
   *  mode where the WebPlayer SDK can't drive a second webview. */
  miniPlayerAvailable: boolean;
  /** When `true`, A-B loop is pinned as a primary button in the bar
   *  and the overflow menu doesn't duplicate it. */
  pinAbLoop: boolean;
  /** When `true`, sleep timer is pinned as a primary button in the
   *  bar and the overflow menu doesn't duplicate it. */
  pinSleepTimer: boolean;
  sleepTimer: {
    status: SleepTimerStatus;
    onSetDuration: (minutes: number) => void;
    onSetEndOfTrack: () => void;
    onCancel: () => void;
  };
  onOpenFullscreen: () => void;
  onOpenMiniPlayer: () => void;
}

/**
 * Overflow popover for the player bar's secondary actions. By default
 * hosts Fullscreen, Mini-player, Sleep timer (inline panel) and A-B
 * loop. Users can pin Sleep timer / A-B loop to the bar via Settings,
 * in which case those entries are omitted from this menu.
 */
export function MoreActionsMenu({
  miniPlayerAvailable,
  pinAbLoop,
  pinSleepTimer,
  sleepTimer,
  onOpenFullscreen,
  onOpenMiniPlayer,
}: MoreActionsMenuProps) {
  const { t } = useTranslation();
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

  const handle = (cb: () => void) => () => {
    setIsOpen(false);
    cb();
  };

  const showSleepInMenu = !pinSleepTimer;
  const showAbInMenu = !pinAbLoop;

  const sleepArmed = sleepTimer.status.kind !== "off";
  const sleepBadge =
    sleepTimer.status.kind === "duration"
      ? formatRemaining(sleepTimer.status.remainingMs)
      : sleepTimer.status.kind === "end-of-track"
        ? t("sleepTimer.endOfTrackBadge")
        : null;

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
        aria-haspopup="menu"
        aria-expanded={isOpen}
        title={t("playerBar.moreActions")}
        className={`relative p-2 rounded-lg transition-colors ${
          isOpen
            ? "text-emerald-500"
            : sleepArmed && showSleepInMenu
              ? "text-emerald-500 hover:text-emerald-400"
              : "text-zinc-400 hover:text-zinc-800 dark:hover:text-white"
        }`}
      >
        <MoreHorizontal size={20} />
        {/* Surface the sleep-timer countdown on the "..." trigger when
            the timer is armed AND lives in the overflow menu — without
            this, the user loses the live countdown the moment they
            unpin the feature. */}
        {sleepBadge && showSleepInMenu && (
          <span className="absolute -top-1 -right-1 px-1 min-w-[18px] h-[16px] flex items-center justify-center rounded-full bg-emerald-500 text-white text-[9px] font-bold leading-none">
            {sleepBadge}
          </span>
        )}
      </button>

      {isOpen && (
        <div
          role="menu"
          aria-label={t("playerBar.moreActions")}
          className="absolute bottom-full right-0 mb-3 w-72 p-1 rounded-xl bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 shadow-xl z-50"
        >
          <button
            type="button"
            role="menuitem"
            onClick={handle(onOpenFullscreen)}
            className="w-full flex items-center gap-3 px-3 py-2 text-sm text-zinc-700 dark:text-zinc-200 rounded-lg hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
          >
            <Maximize2 size={16} className="text-zinc-500" />
            {t("playerBar.openFullscreen")}
          </button>
          {miniPlayerAvailable && (
            <button
              type="button"
              role="menuitem"
              onClick={handle(onOpenMiniPlayer)}
              className="w-full flex items-center gap-3 px-3 py-2 text-sm text-zinc-700 dark:text-zinc-200 rounded-lg hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
            >
              <PictureInPicture2 size={16} className="text-zinc-500" />
              {t("playerBar.miniPlayer")}
            </button>
          )}

          {showAbInMenu && (
            <>
              <div className="my-1 h-px bg-zinc-100 dark:bg-zinc-800" />
              <div className="flex items-center justify-between gap-2 px-3 py-2 rounded-lg hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors">
                <span className="text-sm text-zinc-700 dark:text-zinc-200">
                  {t("playerBar.abLoop")}
                </span>
                <AbLoopButton />
              </div>
            </>
          )}

          {showSleepInMenu && (
            <>
              <div className="my-1 h-px bg-zinc-100 dark:bg-zinc-800" />
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
            </>
          )}
        </div>
      )}
    </div>
  );
}

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

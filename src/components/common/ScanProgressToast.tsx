import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { FolderSearch, X, CheckCircle2 } from "lucide-react";

interface ScanProgress {
  folder_id: number;
  current: number;
  total: number;
  added: number;
  updated: number;
  skipped: number;
  errors: number;
  done: boolean;
}

/**
 * Bottom-right toast that surfaces backend `scan:progress` events.
 *
 * Shows up the moment a library scan starts, ticks the count as files
 * are processed, and flips into a "done" state for ~4 s when the scan
 * finishes (so the user can read the summary). Dismissable manually
 * via the X — the next scan re-opens it automatically.
 *
 * Mounted once at the AppLayout level; no per-page wiring needed
 * because the listener is global.
 */
export function ScanProgressToast() {
  const { t } = useTranslation();
  const [progress, setProgress] = useState<ScanProgress | null>(null);
  const [dismissed, setDismissed] = useState(false);
  const autoHideTimer = useRef<number | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<ScanProgress>("scan:progress", (e) => {
      const next = e.payload;
      setProgress(next);
      setDismissed(false);
      if (autoHideTimer.current != null) {
        window.clearTimeout(autoHideTimer.current);
        autoHideTimer.current = null;
      }
      if (next.done) {
        // Hold the success card for a few seconds so the user has
        // time to read the summary, then fade it out.
        autoHideTimer.current = window.setTimeout(() => {
          setDismissed(true);
        }, 4000);
      }
    })
      .then((fn) => {
        unlisten = fn;
      })
      .catch((err) => console.error("[ScanProgressToast] listen failed", err));
    return () => {
      unlisten?.();
      if (autoHideTimer.current != null) {
        window.clearTimeout(autoHideTimer.current);
      }
    };
  }, []);

  if (progress == null || dismissed) return null;

  const { current, total, added, updated, skipped, done } = progress;
  const percent =
    total > 0 ? Math.min(100, Math.round((current / total) * 100)) : 0;

  return (
    <div
      role="status"
      aria-live="polite"
      className="fixed bottom-28 right-6 z-50 w-80 rounded-2xl border border-zinc-200 dark:border-zinc-700 bg-white/95 dark:bg-zinc-900/95 backdrop-blur shadow-xl p-4 animate-fade-in"
    >
      <div className="flex items-start gap-3">
        <div
          className={`shrink-0 w-9 h-9 rounded-full flex items-center justify-center ${
            done
              ? "bg-emerald-500/15 text-emerald-500"
              : "bg-emerald-500/10 text-emerald-500"
          }`}
        >
          {done ? (
            <CheckCircle2 size={18} />
          ) : (
            <FolderSearch size={18} className="animate-pulse" />
          )}
        </div>
        <div className="flex-1 min-w-0">
          <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
            {done
              ? t("scanProgress.doneTitle")
              : t("scanProgress.runningTitle")}
          </div>
          <div className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">
            {done
              ? t("scanProgress.doneSubtitle", { added, updated, skipped })
              : t("scanProgress.runningSubtitle", {
                  current,
                  total,
                })}
          </div>
          {!done && (
            <div className="mt-2 h-1.5 w-full rounded-full bg-zinc-200 dark:bg-zinc-800 overflow-hidden">
              <div
                className="h-full bg-emerald-500 transition-[width] duration-200"
                style={{ width: `${percent}%` }}
              />
            </div>
          )}
        </div>
        <button
          type="button"
          onClick={() => setDismissed(true)}
          aria-label={t("common.close")}
          className="shrink-0 p-1 rounded hover:bg-zinc-100 dark:hover:bg-zinc-800 text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors"
        >
          <X size={14} />
        </button>
      </div>
    </div>
  );
}

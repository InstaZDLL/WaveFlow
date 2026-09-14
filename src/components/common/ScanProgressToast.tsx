import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { X, CheckCircle2, AlertTriangle } from "lucide-react";

interface ScanProgress {
  folder_id: number;
  current: number;
  total: number;
  added: number;
  updated: number;
  skipped: number;
  errors: number;
  done: boolean;
  /** The user stopped it. The counts below are still what the run
   *  managed to write, and they are still worth showing. */
  cancelled?: boolean;
  current_dir?: string | null;
}

/**
 * Bottom-right toast that surfaces backend `scan:progress` events.
 *
 * Appears when a scan *finishes*, and holds the summary for ~4 s so
 * the user can read it. Live progress moved to the task status bar
 * with #601, which lists every long operation in one place; two bars
 * counting the same files is the inconsistency that issue was about.
 * Dismissable manually via the X — the next scan re-opens it.
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
      // Only the terminal event. A running scan's ticks are the status
      // bar's job, so they have nothing to render here -- but taking
      // them would blank a summary still on screen and cancel the timer
      // holding it, losing the one line that reports per-file failures
      // to a scan the user has not finished reading. The watcher starts
      // a rescan on its own, so this is not a rare sequence.
      if (!next.done) return;
      setProgress(next);
      setDismissed(false);
      if (autoHideTimer.current != null) {
        window.clearTimeout(autoHideTimer.current);
        autoHideTimer.current = null;
      }
      // Hold the success card for a few seconds so the user has time
      // to read the summary, then fade it out.
      autoHideTimer.current = window.setTimeout(() => {
        setDismissed(true);
      }, 4000);
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

  // What the status bar cannot show is the *outcome*: a row vanishes
  // when its task ends, and "412 added, 3 errors" is the part worth
  // reading. `done` is still tested here as well as in the listener --
  // the state can only hold a terminal event now, and this says so at
  // the place that depends on it.
  if (progress == null || dismissed || !progress.done) return null;

  const { current, total, added, updated, skipped, errors, cancelled } =
    progress;
  // How far the walk got. Only meaningful for a scan that stopped:
  // a completed one is at 100% by definition, and says so in words.
  const percent =
    total > 0 ? Math.min(100, Math.round((current / total) * 100)) : 0;
  // Per-file failures. The scan reports itself as finished either
  // way, so this card is the only signal the user gets.
  const partial = errors > 0;

  return (
    <div
      role="status"
      aria-live="polite"
      className="fixed bottom-28 right-6 z-50 w-80 rounded-2xl border border-zinc-200 dark:border-zinc-700 bg-white/95 dark:bg-zinc-900/95 backdrop-blur shadow-xl p-4 animate-fade-in"
    >
      <div className="flex items-start gap-3">
        <div
          className={`shrink-0 w-9 h-9 rounded-full flex items-center justify-center ${
            partial
              ? "bg-amber-500/15 text-amber-500"
              : "bg-emerald-500/15 text-emerald-500"
          }`}
        >
          {partial ? <AlertTriangle size={18} /> : <CheckCircle2 size={18} />}
        </div>
        <div className="flex-1 min-w-0">
          {/* A scan that hit errors must not read as a plain success:
              the backend reports it as complete either way, so the
              failure count takes the headline (and the icon goes amber)
              rather than trailing a green "Scan complete". The
              added/updated/skipped line stays below as context. */}
          <div
            className={`text-sm font-semibold ${
              partial
                ? "text-amber-700 dark:text-amber-500"
                : "text-zinc-900 dark:text-zinc-100"
            }`}
          >
            {/* "Scan complete" for a walk the user stopped halfway
                would tell them their library had been gone through when
                it had not -- and this card is the only outcome they
                get, since the status-bar row leaves with the task. */}
            {partial
              ? t("scanProgress.doneErrors", { count: errors })
              : cancelled
                ? t("scanProgress.cancelledTitle")
                : t("scanProgress.doneTitle")}
          </div>
          <div className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">
            {/* What the run managed to write, which is committed and
                correct whether or not it reached the end. */}
            {t("scanProgress.doneSubtitle", { added, updated, skipped })}
          </div>
          {cancelled && (
            <>
              {/* The one place the distance covered is worth a bar: a
                  finished scan is full by definition, so this renders
                  only for a stopped one. */}
              <div className="mt-2 h-1.5 w-full rounded-full bg-zinc-200 dark:bg-zinc-800 overflow-hidden">
                <div
                  className="h-full bg-emerald-500"
                  style={{ width: `${percent}%` }}
                />
              </div>
              <div className="mt-1.5 text-[11px] text-zinc-400 dark:text-zinc-500">
                {t("scanProgress.runningSubtitle", { current, total })}
              </div>
            </>
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

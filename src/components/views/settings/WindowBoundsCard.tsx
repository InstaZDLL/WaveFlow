import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Maximize2 } from "lucide-react";
import { clearMainWindowBounds } from "../../../lib/tauri/preferences";
import { suppressMainWindowBoundsWrites } from "../../../lib/mainWindowBoundsGuard";

/**
 * Settings → Appearance action that forgets the persisted main-window
 * size + position (issue #339 follow-up). Handy when the saved bounds
 * ended up awkward (a disconnected monitor, a tiny leftover size) — the
 * next launch reopens at the manifest default. The reset only takes
 * effect on the next launch because the live window keeps its current
 * geometry until then.
 */
export function WindowBoundsCard() {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(false);
  const [error, setError] = useState(false);

  const handleReset = async () => {
    if (busy) return;
    setBusy(true);
    setDone(false);
    setError(false);
    try {
      // Suppress the debounced bounds writer *before* deleting the row so a
      // save scheduled by a just-completed move/resize can't recreate it
      // after the delete (#362).
      suppressMainWindowBoundsWrites();
      await clearMainWindowBounds();
      setDone(true);
      window.setTimeout(() => setDone(false), 2500);
    } catch (err) {
      console.error("[WindowBoundsCard] reset failed", err);
      setError(true);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section aria-label={t("settings.windowBounds.title")} className="px-4 py-3">
      <div className="flex items-start justify-between gap-3">
        <span className="flex items-start gap-3 min-w-0">
          <Maximize2
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.windowBounds.title")}
            </span>
            <span className="block text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed mt-0.5">
              {t("settings.windowBounds.subtitle")}
            </span>
            {error && (
              <span
                role="alert"
                className="block text-xs text-red-500 leading-relaxed mt-1"
              >
                {t("settings.windowBounds.error")}
              </span>
            )}
          </span>
        </span>
        <button
          type="button"
          onClick={() => void handleReset()}
          disabled={busy}
          className="shrink-0 inline-flex items-center gap-1.5 rounded-full border border-zinc-200 dark:border-zinc-700 px-3 py-1.5 text-xs font-medium text-zinc-700 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors disabled:opacity-50"
        >
          {done ? (
            <>
              <Check size={14} className="text-emerald-500" aria-hidden="true" />
              {t("settings.windowBounds.done")}
            </>
          ) : (
            t("settings.windowBounds.reset")
          )}
        </button>
      </div>
    </section>
  );
}

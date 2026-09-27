import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, PictureInPicture2 } from "lucide-react";

/**
 * Settings → Appearance action that puts the mini-player back to its
 * default size in the default corner, the same reset a double-click on
 * its drag handle does. Unlike the main window's reset beside it, this
 * one applies at once: a mini-player that is open moves straight there,
 * and a closed one opens there next time.
 */
export function MiniPlayerBoundsCard() {
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
      // Loaded on demand, as the player bar does to open it.
      const { resetMiniPlayerBounds } = await import("../../../lib/miniPlayer");
      await resetMiniPlayerBounds();
      setDone(true);
      window.setTimeout(() => setDone(false), 2500);
    } catch (err) {
      console.error("[MiniPlayerBoundsCard] reset failed", err);
      setError(true);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section
      aria-label={t("settings.miniPlayerBounds.title")}
      className="px-4 py-3"
    >
      <div className="flex items-start justify-between gap-3">
        <span className="flex items-start gap-3 min-w-0">
          <PictureInPicture2
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.miniPlayerBounds.title")}
            </span>
            <span className="block text-xs mt-0.5 settings-description">
              {t("settings.miniPlayerBounds.subtitle")}
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
              <Check
                size={14}
                className="text-emerald-500"
                aria-hidden="true"
              />
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

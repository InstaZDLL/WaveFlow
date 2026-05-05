import { useTranslation } from "react-i18next";
import { ArrowDownToLine, CheckCircle2, X } from "lucide-react";
import { useUpdater } from "../../hooks/useUpdater";

/**
 * Bottom-right toast for the auto-updater. Mirrors the styling of
 * `LastfmReauthBanner` so the two banners stack cleanly when both
 * happen to be visible. Hidden whenever the updater is idle (no
 * update found, or the user dismissed the prompt).
 */
export function UpdateBanner() {
  const { t } = useTranslation();
  const { state, install, dismiss } = useUpdater();

  if (state.kind === "idle") return null;

  const tone =
    state.kind === "error"
      ? "border-rose-200 dark:border-rose-500/30"
      : "border-emerald-200 dark:border-emerald-500/30";

  return (
    <div
      role="status"
      className={`fixed bottom-4 right-4 z-100 max-w-sm rounded-2xl border bg-white shadow-xl dark:bg-zinc-900 animate-fade-in ${tone}`}
    >
      <div className="flex items-start gap-3 p-4">
        <div className="shrink-0 mt-0.5 text-emerald-500">
          {state.kind === "installed" ? (
            <CheckCircle2 size={20} />
          ) : (
            <ArrowDownToLine size={20} />
          )}
        </div>
        <div className="flex-1 min-w-0">
          {state.kind === "available" && (
            <>
              <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
                {t("updater.available.title", { version: state.version })}
              </div>
              <div className="text-xs text-zinc-500 dark:text-zinc-400 mt-1">
                {t("updater.available.message")}
              </div>
              <div className="mt-3 flex items-center gap-2">
                <button
                  type="button"
                  onClick={install}
                  className="inline-flex items-center px-3 py-1.5 rounded-lg text-xs font-medium bg-emerald-500 text-white hover:bg-emerald-600 transition-colors"
                >
                  {t("updater.available.action")}
                </button>
                <button
                  type="button"
                  onClick={dismiss}
                  className="inline-flex items-center px-3 py-1.5 rounded-lg text-xs font-medium text-zinc-600 hover:text-zinc-900 dark:text-zinc-300 dark:hover:text-zinc-100 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
                >
                  {t("updater.available.later")}
                </button>
              </div>
            </>
          )}
          {state.kind === "downloading" && (
            <>
              <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
                {t("updater.downloading.title")}
              </div>
              <div className="text-xs text-zinc-500 dark:text-zinc-400 mt-1">
                {Math.round(state.progress * 100)}%
              </div>
              <div className="mt-2 h-1 rounded-full overflow-hidden bg-zinc-200 dark:bg-zinc-700">
                <div
                  className="h-full bg-emerald-500 transition-[width] duration-150"
                  style={{ width: `${Math.round(state.progress * 100)}%` }}
                />
              </div>
            </>
          )}
          {state.kind === "installed" && (
            <>
              <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
                {t("updater.installed.title")}
              </div>
              <div className="text-xs text-zinc-500 dark:text-zinc-400 mt-1">
                {t("updater.installed.message")}
              </div>
            </>
          )}
          {state.kind === "error" && (
            <>
              <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
                {t("updater.error.title")}
              </div>
              <div className="text-xs text-zinc-500 dark:text-zinc-400 mt-1 break-words">
                {state.message}
              </div>
            </>
          )}
        </div>
        <button
          type="button"
          onClick={dismiss}
          aria-label={t("common.close")}
          className="shrink-0 p-1 -mr-1 rounded-md text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
        >
          <X size={14} />
        </button>
      </div>
    </div>
  );
}

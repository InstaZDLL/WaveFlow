import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ShieldAlert, X } from "lucide-react";
import { useSkin } from "../../hooks/useSkin";

const RECOVERING =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("safe-mode") === "1";

/** Explain why the user's chosen skin became Studio on this launch. */
export function SkinRecoveryBanner() {
  const { t } = useTranslation();
  const { skinError } = useSkin();
  const [visible, setVisible] = useState(RECOVERING);
  if (!visible) return null;

  return (
    <div
      role="status"
      className="fixed top-16 right-4 z-100 max-w-sm rounded-2xl border border-amber-200 bg-white shadow-xl dark:border-amber-500/30 dark:bg-zinc-900 motion-safe:animate-fade-in"
    >
      <div className="flex items-start gap-3 p-4">
        <ShieldAlert
          size={20}
          className="mt-0.5 shrink-0 text-amber-500"
          aria-hidden="true"
        />
        <div className="min-w-0 flex-1">
          <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
            {t("skinRecovery.title")}
          </div>
          <div className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
            {t("skinRecovery.message")}
          </div>
          {skinError && (
            <div
              role="alert"
              className="mt-2 text-xs text-red-600 dark:text-red-400"
            >
              {t("skinRecovery.saveFailed")}
            </div>
          )}
        </div>
        <button
          type="button"
          onClick={() => setVisible(false)}
          aria-label={t("common.close")}
          className="shrink-0 p-1 -mr-1 rounded-md text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
        >
          <X size={14} aria-hidden="true" />
        </button>
      </div>
    </div>
  );
}

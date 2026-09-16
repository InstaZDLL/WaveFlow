import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { MonitorCog, X } from "lucide-react";

import { rendererStatus } from "../../lib/tauri/renderer";

interface SoftwareRenderingBannerProps {
  onGoToSettings: () => void;
}

/**
 * Says so when the app fell back to software rendering (#595).
 *
 * The fallback exists because a GPU that cannot paint leaves a blank
 * window with no message and no way back. Recovering from that silently
 * trades one confusion for another: the app works, and is slower, and
 * nothing said why. So the launch that recovers explains itself once.
 *
 * **Only the fallback, never a choice.** A mode the user forced through
 * `WAVEFLOW_RENDERER` is not news to them, and a launch already
 * remembering a working software mode has said this before — that one
 * lives in Settings, where it can be read when wanted rather than
 * announced again on every start.
 *
 * Dismissable, and stays until dismissed: unlike the Last.fm banner
 * this is not a transient event, and a message about why the interface
 * is slow should not vanish while the user is working out whether it
 * is.
 */
export function SoftwareRenderingBanner({
  onGoToSettings,
}: SoftwareRenderingBannerProps) {
  const { t } = useTranslation();
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    let cancelled = false;
    rendererStatus()
      .then((status) => {
        if (cancelled) return;
        setVisible(status?.reason === "previous-launch-never-painted");
      })
      .catch((err) => {
        console.error("[SoftwareRenderingBanner] status failed", err);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (!visible) return null;

  return (
    <div
      role="status"
      className="fixed bottom-4 right-4 z-100 max-w-sm rounded-2xl border border-amber-200 bg-white shadow-xl dark:border-amber-500/30 dark:bg-zinc-900 animate-fade-in"
    >
      <div className="flex items-start gap-3 p-4">
        <div className="shrink-0 mt-0.5 text-amber-500">
          <MonitorCog size={20} aria-hidden="true" />
        </div>
        <div className="flex-1 min-w-0">
          <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
            {t("rendering.banner.title")}
          </div>
          <div className="text-xs text-zinc-500 dark:text-zinc-400 mt-1">
            {t("rendering.banner.message")}
          </div>
          <button
            type="button"
            onClick={() => {
              onGoToSettings();
              setVisible(false);
            }}
            className="mt-3 inline-flex items-center px-3 py-1.5 rounded-lg text-xs font-medium bg-emerald-500 text-white hover:bg-emerald-600 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
          >
            {t("rendering.banner.action")}
          </button>
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

import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { MonitorCog } from "lucide-react";

import {
  rendererRetryGpu,
  rendererStatus,
  type RendererStatus,
} from "../../../lib/tauri/renderer";

/**
 * How the interface is being drawn, and the way back from the software
 * fallback (#595).
 *
 * Lives in Diagnostics rather than in Appearance because it is not a
 * preference: nobody picks this, they are told it. What is offered is
 * one action — forget the fallback — and it is offered only when there
 * is something to forget.
 */
export function RenderingCard() {
  const { t } = useTranslation();
  const [status, setStatus] = useState<RendererStatus | null>(null);
  const [retried, setRetried] = useState(false);

  useEffect(() => {
    let cancelled = false;
    rendererStatus()
      .then((value) => {
        if (!cancelled) setStatus(value);
      })
      .catch((err) => console.error("[RenderingCard] status failed", err));
    return () => {
      cancelled = true;
    };
  }, []);

  const handleRetry = useCallback(async () => {
    try {
      await rendererRetryGpu();
      setRetried(true);
    } catch (err) {
      console.error("[RenderingCard] retry failed", err);
    }
  }, []);

  // The decision never ran — no app-data directory — so there is
  // nothing true to say. Better than a card claiming a mode it does not
  // know.
  if (!status) return null;

  return (
    <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
      <div className="flex items-center justify-between gap-4">
        <div className="flex items-center space-x-4 min-w-0">
          <MonitorCog
            size={20}
            className="text-zinc-400 shrink-0"
            aria-hidden="true"
          />
          <div className="min-w-0">
            <div className="text-sm font-medium text-zinc-900 dark:text-white">
              {t("rendering.card.title")}
            </div>
            <div className="text-xs text-zinc-400">
              {t(`rendering.mode.${status.mode}`)} ·{" "}
              {t(`rendering.reason.${status.reason}`)}
            </div>
          </div>
        </div>
        {status.canRetryGpu && (
          <div className="shrink-0">
            {retried ? (
              <span className="text-xs text-emerald-600 dark:text-emerald-400">
                {t("rendering.card.restartToApply")}
              </span>
            ) : (
              <button
                type="button"
                onClick={handleRetry}
                className="px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
              >
                {t("rendering.card.retryGpu")}
              </button>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

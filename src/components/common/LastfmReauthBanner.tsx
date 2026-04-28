import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { AlertTriangle, X } from "lucide-react";

interface LastfmReauthBannerProps {
  onGoToSettings: () => void;
}

/**
 * Toast-style banner shown when the scrobble worker (or
 * `track.updateNowPlaying`) discovers that the cached Last.fm
 * session is no longer valid. The backend has already wiped the
 * `auth_credential` row + the pending scrobbles by the time this
 * fires; the banner just nudges the user toward Settings to sign
 * in again.
 *
 * Visible for 30 s on each event, dismissable via the X button or
 * by clicking the "Open settings" call-to-action.
 */
export function LastfmReauthBanner({ onGoToSettings }: LastfmReauthBannerProps) {
  const { t } = useTranslation();
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let timer: number | null = null;
    listen("lastfm:reauth-required", () => {
      setVisible(true);
      if (timer != null) {
        window.clearTimeout(timer);
      }
      timer = window.setTimeout(() => setVisible(false), 30_000);
    })
      .then((un) => {
        unlisten = un;
      })
      .catch((err) => {
        console.error("[LastfmReauthBanner] listen failed", err);
      });
    return () => {
      if (unlisten) unlisten();
      if (timer != null) window.clearTimeout(timer);
    };
  }, []);

  if (!visible) return null;

  return (
    <div
      role="status"
      className="fixed bottom-4 right-4 z-100 max-w-sm rounded-2xl border border-rose-200 bg-white shadow-xl dark:border-rose-500/30 dark:bg-zinc-900 animate-fade-in"
    >
      <div className="flex items-start gap-3 p-4">
        <div className="shrink-0 mt-0.5 text-rose-500">
          <AlertTriangle size={20} />
        </div>
        <div className="flex-1 min-w-0">
          <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
            {t("lastfmReauth.title")}
          </div>
          <div className="text-xs text-zinc-500 dark:text-zinc-400 mt-1">
            {t("lastfmReauth.message")}
          </div>
          <button
            type="button"
            onClick={() => {
              onGoToSettings();
              setVisible(false);
            }}
            className="mt-3 inline-flex items-center px-3 py-1.5 rounded-lg text-xs font-medium bg-emerald-500 text-white hover:bg-emerald-600 transition-colors"
          >
            {t("lastfmReauth.action")}
          </button>
        </div>
        <button
          type="button"
          onClick={() => setVisible(false)}
          aria-label={t("common.close")}
          className="shrink-0 p-1 -mr-1 rounded-md text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
        >
          <X size={14} />
        </button>
      </div>
    </div>
  );
}

import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Headphones } from "lucide-react";

import { useProfile } from "../../../hooks/useProfile";
import { openUrl } from "../../../lib/tauri/external";
import {
  listenbrainzConnect,
  listenbrainzDisconnect,
  listenbrainzGetStatus,
  type ListenBrainzStatus,
} from "../../../lib/tauri/integration";

/** Where a ListenBrainz user finds their token. */
const TOKEN_PAGE = "https://listenbrainz.org/settings/";

/**
 * Settings → Services row for ListenBrainz, the open counterpart to
 * Last.fm. One field: the user token from their ListenBrainz profile —
 * no API key, no password. The token is checked with ListenBrainz before
 * it is kept, and kept per profile, so the status is read again when the
 * profile changes.
 */
export function ListenBrainzCard() {
  const { t } = useTranslation();
  const { activeProfile } = useProfile();
  const [status, setStatus] = useState<ListenBrainzStatus | null>(null);
  const [token, setToken] = useState("");
  const [showToken, setShowToken] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    listenbrainzGetStatus()
      .then((s) => {
        if (!cancelled) setStatus(s);
      })
      .catch((err) => console.error("[ListenBrainzCard] status failed", err));
    return () => {
      cancelled = true;
    };
  }, [activeProfile?.id]);

  const connect = async () => {
    if (busy || !token.trim()) return;
    setBusy(true);
    setError(null);
    try {
      setStatus(await listenbrainzConnect(token.trim()));
      setToken("");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  const disconnect = async () => {
    try {
      await listenbrainzDisconnect();
      setStatus({ connected: false, username: null });
    } catch (err) {
      console.error("[ListenBrainzCard] disconnect failed", err);
    }
  };

  return (
    <div className="settings-block py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
      <div className="flex items-start space-x-4">
        <Headphones
          size={20}
          className="text-zinc-400 mt-0.5"
          aria-hidden="true"
        />
        <div className="flex-1 min-w-0">
          <div className="text-sm font-medium text-zinc-900 dark:text-white">
            ListenBrainz
          </div>
          <div className="text-xs mb-3 settings-description">
            {t("settings.integrations.listenbrainz.subtitle")}
          </div>

          {status?.connected ? (
            <div className="flex items-center justify-between">
              <div className="text-xs">
                <span className="text-zinc-500">
                  {t("settings.integrations.lastfm.connectedAs")}{" "}
                </span>
                <span className="font-medium text-emerald-500">
                  {status.username}
                </span>
              </div>
              <button
                type="button"
                onClick={disconnect}
                className="px-3 py-1.5 rounded-lg text-xs font-medium border border-zinc-200 bg-white text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors"
              >
                {t("settings.integrations.lastfm.disconnect")}
              </button>
            </div>
          ) : (
            <div className="space-y-2">
              <div className="text-xs text-zinc-500">
                {t("settings.integrations.listenbrainz.tokenPrompt")}{" "}
                <button
                  type="button"
                  onClick={() =>
                    openUrl(TOKEN_PAGE).catch((err) =>
                      console.error("[ListenBrainzCard] open failed", err),
                    )
                  }
                  className="font-medium text-emerald-600 dark:text-emerald-400 hover:underline"
                >
                  {t("settings.integrations.listenbrainz.getToken")}
                </button>
              </div>
              <div className="flex items-center space-x-2">
                <input
                  type={showToken ? "text" : "password"}
                  value={token}
                  onChange={(e) => {
                    setToken(e.target.value);
                    setError(null);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void connect();
                  }}
                  placeholder={t(
                    "settings.integrations.listenbrainz.tokenPlaceholder",
                  )}
                  aria-label={t(
                    "settings.integrations.listenbrainz.tokenPlaceholder",
                  )}
                  autoComplete="off"
                  spellCheck={false}
                  className="flex-1 min-w-0 px-3 py-2 rounded-xl text-sm font-mono bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500"
                />
                <button
                  type="button"
                  onClick={() => setShowToken((v) => !v)}
                  className="px-3 py-2 rounded-xl text-xs font-medium border border-zinc-200 bg-white text-zinc-600 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors"
                >
                  {showToken
                    ? t("settings.integrations.lastfm.hide")
                    : t("settings.integrations.lastfm.show")}
                </button>
                <button
                  type="button"
                  onClick={() => void connect()}
                  disabled={busy || !token.trim()}
                  className="px-4 py-2 rounded-xl text-sm font-medium bg-emerald-500 text-white hover:bg-emerald-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                >
                  {busy
                    ? t("settings.integrations.lastfm.connecting")
                    : t("settings.integrations.lastfm.connect")}
                </button>
              </div>
              {error && <div className="text-xs text-rose-500">{error}</div>}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

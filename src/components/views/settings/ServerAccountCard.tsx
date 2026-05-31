import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { CheckCircle2, ExternalLink, Server } from "lucide-react";
import {
  serverGetStatus,
  serverOpenLoginBrowser,
  serverSetToken,
  serverSetUrl,
  serverSignOut,
  type ServerStatus,
} from "../../../lib/tauri/serverAuth";

/**
 * Settings → Intégrations → "Compte serveur WaveFlow" card.
 *
 * Phase 1.f.desktop.1 — foundational binding to a `waveflow-server`
 * deployment. The UI surfaces two state knobs:
 *
 * - **Server URL** (app-wide, persisted in `app_setting`): the base
 *   URL the desktop will hit for `/api/v1/*` calls.
 * - **JWT** (per-profile, persisted in `auth_credential`): pasted
 *   from the browser sign-in. The polished local-loopback OAuth flow
 *   ships in `1.f.desktop.1b`.
 *
 * The card is intentionally stateless beyond its own form fields —
 * every action invokes a Tauri command and re-reads the snapshot, so
 * a parallel change from another window (or a future profile-switch
 * hook) stays in sync.
 */
export function ServerAccountCard() {
  const { t } = useTranslation();
  const [status, setStatus] = useState<ServerStatus | null>(null);
  const [urlDraft, setUrlDraft] = useState("");
  const [tokenDraft, setTokenDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Initial load. The `cancelled` flag pattern keeps the lint
  // rule (`react-hooks/set-state-in-effect`) happy — the setState
  // calls live inside an async callback, not directly in the effect
  // body, and the flag guards against a setState-after-unmount.
  useEffect(() => {
    let cancelled = false;
    serverGetStatus()
      .then((next) => {
        if (cancelled) return;
        setStatus(next);
        setUrlDraft((current) => (current ? current : (next.url ?? "")));
      })
      .catch((err) => {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const handleSaveUrl = useCallback(async () => {
    setError(null);
    setBusy(true);
    try {
      const next = await serverSetUrl(urlDraft);
      setStatus(next);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [urlDraft]);

  const handleSaveToken = useCallback(async () => {
    setError(null);
    setBusy(true);
    try {
      const next = await serverSetToken(tokenDraft);
      setStatus(next);
      setTokenDraft("");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [tokenDraft]);

  const handleSignOut = useCallback(async () => {
    setError(null);
    setBusy(true);
    try {
      const next = await serverSignOut();
      setStatus(next);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, []);

  const handleOpenLogin = useCallback(async () => {
    setError(null);
    try {
      await serverOpenLoginBrowser();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  const signedIn = status?.signed_in ?? false;
  const urlConfigured = Boolean(status?.url);

  return (
    <section
      aria-label={t("settings.serverAccount.title")}
      className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors"
    >
      <div className="flex items-start space-x-4">
        <Server size={20} className="text-zinc-400 mt-0.5" aria-hidden="true" />
        <div className="flex-1 min-w-0">
          <div className="flex items-center justify-between gap-3">
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.serverAccount.title")}
              </div>
              <div className="text-xs text-zinc-400">
                {t("settings.serverAccount.subtitle")}
              </div>
            </div>
            {signedIn && (
              <span
                className="inline-flex items-center gap-1 text-xs font-medium text-emerald-600 dark:text-emerald-400"
                aria-live="polite"
              >
                <CheckCircle2 size={14} aria-hidden="true" />
                {t("settings.serverAccount.signedIn")}
              </span>
            )}
          </div>

          {/* Server URL */}
          <label className="block mt-4">
            <span className="block text-xs font-medium text-zinc-600 dark:text-zinc-300 mb-1">
              {t("settings.serverAccount.urlLabel")}
            </span>
            <div className="flex items-center space-x-2">
              <input
                type="url"
                value={urlDraft}
                onChange={(e) => {
                  setUrlDraft(e.target.value);
                }}
                placeholder={t("settings.serverAccount.urlPlaceholder")}
                spellCheck={false}
                autoComplete="off"
                disabled={busy}
                className="flex-1 px-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500 disabled:opacity-60"
              />
              <button
                type="button"
                onClick={() => {
                  void handleSaveUrl();
                }}
                disabled={busy}
                className="px-3 py-2 rounded-xl text-sm font-medium bg-emerald-500 text-white hover:bg-emerald-600 transition-colors disabled:opacity-60 disabled:cursor-not-allowed"
              >
                {t("settings.serverAccount.urlSave")}
              </button>
            </div>
          </label>

          {/* Browser sign-in shortcut + JWT paste */}
          <div className="mt-4">
            <div className="flex items-center justify-between gap-3 mb-2">
              <span className="text-xs font-medium text-zinc-600 dark:text-zinc-300">
                {t("settings.serverAccount.tokenLabel")}
              </span>
              <button
                type="button"
                onClick={() => {
                  void handleOpenLogin();
                }}
                disabled={!urlConfigured}
                className="inline-flex items-center gap-1 text-xs font-medium text-emerald-700 dark:text-emerald-400 hover:underline disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <ExternalLink size={12} aria-hidden="true" />
                {t("settings.serverAccount.openLogin")}
              </button>
            </div>
            <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-2">
              {t("settings.serverAccount.tokenHint")}
            </p>
            <div className="flex items-start space-x-2">
              <textarea
                value={tokenDraft}
                onChange={(e) => {
                  setTokenDraft(e.target.value);
                }}
                placeholder={t("settings.serverAccount.tokenPlaceholder")}
                spellCheck={false}
                autoComplete="off"
                rows={3}
                disabled={busy}
                className="flex-1 px-3 py-2 rounded-xl text-xs font-mono bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500 disabled:opacity-60 resize-none"
              />
              <button
                type="button"
                onClick={() => {
                  void handleSaveToken();
                }}
                disabled={busy || tokenDraft.trim().length === 0}
                className="px-3 py-2 rounded-xl text-sm font-medium bg-emerald-500 text-white hover:bg-emerald-600 transition-colors disabled:opacity-60 disabled:cursor-not-allowed"
              >
                {t("settings.serverAccount.tokenSave")}
              </button>
            </div>
          </div>

          {signedIn && (
            <div className="mt-4 flex justify-end">
              <button
                type="button"
                onClick={() => {
                  void handleSignOut();
                }}
                disabled={busy}
                className="px-3 py-2 rounded-xl text-sm font-medium bg-zinc-200 text-zinc-800 hover:bg-zinc-300 transition-colors dark:bg-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-600 disabled:opacity-60"
              >
                {t("settings.serverAccount.signOut")}
              </button>
            </div>
          )}

          {error && (
            <p
              role="alert"
              className="mt-3 text-xs text-red-600 dark:text-red-400"
            >
              {error}
            </p>
          )}
        </div>
      </div>
    </section>
  );
}

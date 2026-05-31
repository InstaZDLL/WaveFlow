import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { CheckCircle2, ExternalLink, LogIn, Server } from "lucide-react";
import {
  serverBeginLoopbackLogin,
  serverGetStatus,
  serverOpenLoginBrowser,
  serverSetToken,
  serverSetUrl,
  serverSetWebUrl,
  serverSignOut,
  syncGetMode,
  syncSetMode,
  type ServerStatus,
  type SyncMode,
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
  const [webUrlDraft, setWebUrlDraft] = useState("");
  const [tokenDraft, setTokenDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [loggingIn, setLoggingIn] = useState(false);
  const [mode, setMode] = useState<SyncMode | null>(null);
  const [modeBusy, setModeBusy] = useState(false);

  // Initial load. The `cancelled` flag pattern keeps the lint
  // rule (`react-hooks/set-state-in-effect`) happy — the setState
  // calls live inside an async callback, not directly in the effect
  // body, and the flag guards against a setState-after-unmount.
  useEffect(() => {
    let cancelled = false;
    Promise.all([serverGetStatus(), syncGetMode().catch(() => null)])
      .then(([nextStatus, nextMode]) => {
        if (cancelled) return;
        setStatus(nextStatus);
        setUrlDraft((current) => (current ? current : (nextStatus.url ?? "")));
        setWebUrlDraft((current) =>
          current ? current : (nextStatus.web_url ?? ""),
        );
        // `syncGetMode` requires an active profile pool — if it
        // failed (e.g. mid-profile-switch) we just don't render the
        // radio rather than blow up the whole card. The promise
        // above swallows the error so the destructure is safe.
        if (nextMode) {
          setMode(nextMode);
        }
      })
      .catch((err) => {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const handleSetMode = useCallback(async (next: SyncMode) => {
    setError(null);
    setModeBusy(true);
    try {
      const stored = await syncSetMode(next);
      setMode(stored);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setModeBusy(false);
    }
  }, []);

  const handleSaveUrl = useCallback(async () => {
    setError(null);
    setBusy(true);
    try {
      // Normalise client-side so the input mirrors what got persisted
      // (the backend trims too, but reflecting that here keeps the
      // draft and the stored value in sync without a refresh
      // round-trip).
      const normalizedUrl = urlDraft.trim();
      const next = await serverSetUrl(normalizedUrl);
      setStatus(next);
      setUrlDraft(normalizedUrl);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [urlDraft]);

  const handleSaveWebUrl = useCallback(async () => {
    setError(null);
    setBusy(true);
    try {
      const normalized = webUrlDraft.trim();
      const next = await serverSetWebUrl(normalized);
      setStatus(next);
      setWebUrlDraft(normalized);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [webUrlDraft]);

  const handleSaveToken = useCallback(async () => {
    setError(null);
    setBusy(true);
    try {
      // Trim before send so a copy-paste that pulled a trailing
      // newline (or `Bearer ` prefix-style padding the user might
      // accidentally include) doesn't drag spurious bytes into the
      // structural three-segment check.
      const normalizedToken = tokenDraft.trim();
      const next = await serverSetToken(normalizedToken);
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

  const handleOauthLogin = useCallback(async () => {
    setError(null);
    setLoggingIn(true);
    try {
      const next = await serverBeginLoopbackLogin();
      setStatus(next);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoggingIn(false);
    }
  }, []);

  const signedIn = status?.signed_in ?? false;
  const urlConfigured = Boolean(status?.url);
  const webUrlConfigured = Boolean(status?.web_url);

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

          {/* Web URL (for OAuth-loopback handshake) */}
          <label className="block mt-3">
            <span className="block text-xs font-medium text-zinc-600 dark:text-zinc-300 mb-1">
              {t("settings.serverAccount.webUrlLabel")}
            </span>
            <div className="flex items-center space-x-2">
              <input
                type="url"
                value={webUrlDraft}
                onChange={(e) => {
                  setWebUrlDraft(e.target.value);
                }}
                placeholder={t("settings.serverAccount.webUrlPlaceholder")}
                spellCheck={false}
                autoComplete="off"
                disabled={busy}
                className="flex-1 px-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500 disabled:opacity-60"
              />
              <button
                type="button"
                onClick={() => {
                  void handleSaveWebUrl();
                }}
                disabled={busy}
                className="px-3 py-2 rounded-xl text-sm font-medium bg-emerald-500 text-white hover:bg-emerald-600 transition-colors disabled:opacity-60 disabled:cursor-not-allowed"
              >
                {t("settings.serverAccount.urlSave")}
              </button>
            </div>
          </label>

          {/* OAuth-loopback primary action */}
          <div className="mt-4">
            <button
              type="button"
              onClick={() => {
                void handleOauthLogin();
              }}
              disabled={!webUrlConfigured || loggingIn || busy}
              className="inline-flex items-center gap-2 px-4 py-2 rounded-xl text-sm font-medium bg-emerald-500 text-white hover:bg-emerald-600 transition-colors disabled:opacity-60 disabled:cursor-not-allowed"
            >
              <LogIn size={14} aria-hidden="true" />
              {loggingIn
                ? t("settings.serverAccount.loginInProgress")
                : t("settings.serverAccount.signInWithBrowser")}
            </button>
            <p className="mt-2 text-xs text-zinc-500 dark:text-zinc-400">
              {t("settings.serverAccount.signInWithBrowserHint")}
            </p>
          </div>

          {/* Manual paste fallback */}
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

          {/* Sync mode radio — only meaningful once signed in, since
            the queue gate also requires a JWT. Hidden while `mode`
            hydrates so we don't flash an empty radio group. */}
          {signedIn && mode !== null && (
            <fieldset className="mt-4">
              <legend className="text-xs font-medium text-zinc-600 dark:text-zinc-300 mb-2">
                {t("settings.serverAccount.modeLabel")}
              </legend>
              <div className="space-y-1">
                {(["hybrid", "local"] as const).map((value) => {
                  const checked = mode === value;
                  return (
                    <label
                      key={value}
                      className="flex items-start gap-3 px-3 py-2 rounded-lg cursor-pointer hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors"
                    >
                      <input
                        type="radio"
                        name="sync-mode"
                        value={value}
                        checked={checked}
                        disabled={modeBusy}
                        onChange={() => {
                          void handleSetMode(value);
                        }}
                        className="mt-0.5 w-4 h-4 accent-emerald-500 cursor-pointer disabled:opacity-50"
                      />
                      <span className="min-w-0">
                        <span className="block text-sm text-zinc-800 dark:text-zinc-200">
                          {t(`settings.serverAccount.modes.${value}.label`)}
                        </span>
                        <span className="block text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed">
                          {t(`settings.serverAccount.modes.${value}.description`)}
                        </span>
                      </span>
                    </label>
                  );
                })}
              </div>
            </fieldset>
          )}

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

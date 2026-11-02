import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Network, Plus, ShieldAlert, Trash2 } from "lucide-react";

import { useLibrary } from "../../../hooks/useLibrary";
import { useProfile } from "../../../hooks/useProfile";
import { useProfileSetting } from "../../../hooks/useProfileSetting";
import {
  refreshNetworkTracks,
  useNetworkSources,
} from "../../../lib/networkTracks";
import { SETTINGS } from "../../../lib/settingsRegistry";
import {
  announceImport,
  parseFolderOverlap,
  scanFolder,
} from "../../../lib/tauri/library";
import {
  addNetworkSource,
  clearNetworkCache,
  getNetworkCacheInfo,
  NETWORK_CACHE_LIMIT_EVENT,
  notifyNetworkCacheLimitChanged,
  removeNetworkSource,
  setNetworkSourceCredentials,
  testNetworkSource,
  type NetworkSourceInput,
  type NetworkSourceKind,
  type NetworkTestOutcome,
} from "../../../lib/tauri/networkLibrary";

const KINDS: NetworkSourceKind[] = ["webdav", "smb"];
const MIN_CACHE_GB = 1;
const MAX_CACHE_GB = 500;

const EMPTY_FORM: NetworkSourceInput = {
  kind: "webdav",
  name: "",
  endpoint: "",
  share: "",
  root: "",
  username: "",
  password: "",
  allowPlainHttp: false,
};

/** Consent to plain HTTP covers the address it was given about: a box ticked
 *  and then hidden by correcting the address to https:// counts for nothing. */
function withConsent(form: NetworkSourceInput): NetworkSourceInput {
  const plain = form.endpoint.trim().toLowerCase().startsWith("http://");
  return { ...form, allowPlainHttp: plain && form.allowPlainHttp };
}

const inputClass =
  "w-full px-3 py-2 rounded-lg bg-zinc-100 dark:bg-zinc-800 text-sm text-zinc-900 dark:text-white placeholder:text-zinc-400 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500";
const buttonClass =
  "px-3 py-1.5 rounded-lg text-xs font-medium bg-zinc-100 dark:bg-zinc-800 text-zinc-700 dark:text-zinc-200 hover:bg-zinc-200 dark:hover:bg-zinc-700 disabled:opacity-50 transition-colors";

/**
 * Settings → Storage and backups: WebDAV and SMB shares read directly as
 * library folders, without the system mounting them, and the local copies kept
 * of what plays from them.
 */
export function NetworkLibraryCard({ language }: { language: string }) {
  const { t } = useTranslation();
  const sources = useNetworkSources();
  const { selectedLibraryId, libraries, refresh } = useLibrary();
  const { activeProfile } = useProfile();
  const [form, setForm] = useState<NetworkSourceInput | null>(null);
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<NetworkTestOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirmRemove, setConfirmRemove] = useState<number | null>(null);
  const [passwordFor, setPasswordFor] = useState<number | null>(null);
  const [newPassword, setNewPassword] = useState("");
  const [cache, setCache] = useState<{ bytes: number; entries: number } | null>(
    null,
  );

  const cacheLimit = useProfileSetting<number>({
    key: "network.cache_max_gb",
    defaultValue: SETTINGS["network.cache_max_gb"].defaultValue,
    parse: (raw) => {
      const value = Number.parseInt(raw ?? "", 10);
      return Number.isFinite(value)
        ? value
        : SETTINGS["network.cache_max_gb"].defaultValue;
    },
    serialize: String,
    valueType: "int",
    event: NETWORK_CACHE_LIMIT_EVENT,
    label: "NetworkLibraryCard.cacheLimit",
  });

  const readCache = useCallback(() => {
    getNetworkCacheInfo()
      .then(setCache)
      .catch((err) => console.error("[NetworkLibraryCard] cache info", err));
  }, []);
  // The profile decides whose cache this is, and a switch keeps this card
  // mounted.
  useEffect(readCache, [readCache, activeProfile?.id]);

  const plainHttp = form?.endpoint.trim().toLowerCase().startsWith("http://");

  const failure = (test: NetworkTestOutcome) =>
    t(`settings.networkLibrary.failure.${test.code ?? "protocol"}`);

  const onTest = async () => {
    if (!form) return;
    setBusy(true);
    setError(null);
    try {
      setOutcome(await testNetworkSource(withConsent(form)));
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const onAdd = async () => {
    if (!form) return;
    const libraryId = selectedLibraryId ?? libraries[0]?.id;
    if (libraryId == null) {
      setError(t("settings.networkLibrary.noLibrary"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const added = await addNetworkSource(libraryId, withConsent(form));
      setOutcome(added.test);
      if (added.folderId != null) {
        setForm(null);
        setOutcome(null);
        await refreshNetworkTracks();
        // Not awaited by the form: the task row reports it, and a first scan
        // of a share can take a while.
        void scanFolder(added.folderId)
          .then(refresh)
          .catch((err) => setError(String(err)));
      }
    } catch (err) {
      // The same sentence as a local folder refused for the same reason.
      const overlap = parseFolderOverlap(err);
      if (overlap) announceImport({ kind: "overlap", overlap });
      else setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const onRemove = async (id: number) => {
    setBusy(true);
    setError(null);
    try {
      await removeNetworkSource(id);
      setConfirmRemove(null);
      await refreshNetworkTracks();
      await refresh();
      readCache();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const onSavePassword = async (
    id: number,
    username: string | null,
    password: string,
  ) => {
    setBusy(true);
    setError(null);
    try {
      await setNetworkSourceCredentials(id, username, password);
      setPasswordFor(null);
      setNewPassword("");
      await refreshNetworkTracks();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const onClearCache = async () => {
    setBusy(true);
    try {
      await clearNetworkCache();
    } catch (err) {
      setError(String(err));
    } finally {
      readCache();
      setBusy(false);
    }
  };

  // Saved first, then the backend is told: it reads the value from the row.
  const onCacheLimit = async (raw: string) => {
    const value = Number.parseInt(raw, 10);
    if (!Number.isFinite(value)) return;
    await cacheLimit.setValue(
      Math.min(MAX_CACHE_GB, Math.max(MIN_CACHE_GB, value)),
    );
    await notifyNetworkCacheLimitChanged().catch((err) =>
      console.error("[NetworkLibraryCard] cache limit", err),
    );
  };

  const cacheSize = cache
    ? new Intl.NumberFormat(language, {
        style: "unit",
        unit: "gigabyte",
        maximumFractionDigits: 1,
      }).format(cache.bytes / 1024 ** 3)
    : "—";

  return (
    <div className="settings-row flex flex-col gap-3">
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center space-x-4 min-w-0">
          <Network size={16} className="settings-row-icon" aria-hidden="true" />
          <div className="min-w-0">
            <div className="text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.networkLibrary.title")}
            </div>
            <div className="text-xs settings-description">
              {t("settings.networkLibrary.subtitle")}
            </div>
          </div>
        </div>
        {form == null && (
          <button
            type="button"
            className={`${buttonClass} shrink-0 inline-flex items-center gap-1.5`}
            onClick={() => {
              setForm(EMPTY_FORM);
              setOutcome(null);
            }}
          >
            <Plus size={14} aria-hidden="true" />
            {t("settings.networkLibrary.add")}
          </button>
        )}
      </div>

      {sources.map((source) => (
        <div
          key={source.id}
          className="ml-9 rounded-xl border border-zinc-200 dark:border-zinc-800 p-3 flex flex-col gap-2"
        >
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <div className="text-sm text-zinc-900 dark:text-white truncate">
                {source.name}
              </div>
              <div className="text-xs settings-description truncate">
                {source.endpoint}
                {source.share && `/${source.share}`}
                {source.folders.map((f) => ` · /${f.path}`).join("")}
              </div>
            </div>
            <span
              className={`shrink-0 text-xs ${
                source.state === "online" || source.state === "unknown"
                  ? "text-emerald-600 dark:text-emerald-400"
                  : "text-amber-600 dark:text-amber-400"
              }`}
              title={source.lastError ?? undefined}
            >
              {t(`settings.networkLibrary.state.${source.state}`)}
            </span>
          </div>
          {source.plainHttp && (
            <div className="text-xs text-amber-600 dark:text-amber-400 flex items-center gap-1.5">
              <ShieldAlert size={12} aria-hidden="true" />
              {t("settings.networkLibrary.plainHttpBadge")}
            </div>
          )}
          {!source.hasPassword && source.username && (
            <div className="text-xs text-amber-600 dark:text-amber-400">
              {t("settings.networkLibrary.passwordMissing")}
            </div>
          )}
          {passwordFor === source.id ? (
            <div className="flex items-center gap-2">
              <input
                type="password"
                autoComplete="off"
                className={inputClass}
                value={newPassword}
                aria-label={t("settings.networkLibrary.password")}
                placeholder={t("settings.networkLibrary.password")}
                onChange={(e) => setNewPassword(e.target.value)}
              />
              <button
                type="button"
                className={buttonClass}
                disabled={busy || newPassword === ""}
                onClick={() =>
                  void onSavePassword(source.id, source.username, newPassword)
                }
              >
                {t("settings.networkLibrary.save")}
              </button>
              <button
                type="button"
                className={buttonClass}
                onClick={() => {
                  setPasswordFor(null);
                  setNewPassword("");
                }}
              >
                {t("settings.networkLibrary.cancel")}
              </button>
            </div>
          ) : (
            <div className="flex items-center gap-2">
              <button
                type="button"
                className={buttonClass}
                onClick={() => {
                  setPasswordFor(source.id);
                  setNewPassword("");
                }}
              >
                {t("settings.networkLibrary.changePassword")}
              </button>
              <button
                type="button"
                disabled={busy}
                className={`${buttonClass} inline-flex items-center gap-1.5`}
                onClick={() =>
                  confirmRemove === source.id
                    ? void onRemove(source.id)
                    : setConfirmRemove(source.id)
                }
              >
                <Trash2 size={12} aria-hidden="true" />
                {confirmRemove === source.id
                  ? t("settings.networkLibrary.removeConfirm")
                  : t("settings.networkLibrary.remove")}
              </button>
            </div>
          )}
        </div>
      ))}

      {form != null && (
        <div className="ml-9 rounded-xl border border-zinc-200 dark:border-zinc-800 p-3 flex flex-col gap-2">
          {KINDS.length > 1 && (
            <select
              className={inputClass}
              value={form.kind}
              aria-label={t("settings.networkLibrary.kind")}
              onChange={(e) =>
                setForm({ ...form, kind: e.target.value as NetworkSourceKind })
              }
            >
              {KINDS.map((kind) => (
                <option key={kind} value={kind}>
                  {t(`settings.networkLibrary.kinds.${kind}`)}
                </option>
              ))}
            </select>
          )}
          <input
            className={inputClass}
            value={form.name}
            aria-label={t("settings.networkLibrary.name")}
            placeholder={t("settings.networkLibrary.name")}
            onChange={(e) => setForm({ ...form, name: e.target.value })}
          />
          <input
            className={inputClass}
            value={form.endpoint}
            spellCheck={false}
            aria-label={t("settings.networkLibrary.address")}
            placeholder={t(`settings.networkLibrary.addressHint.${form.kind}`)}
            onChange={(e) => setForm({ ...form, endpoint: e.target.value })}
          />
          {form.kind === "smb" && (
            <input
              className={inputClass}
              value={form.share}
              spellCheck={false}
              aria-label={t("settings.networkLibrary.share")}
              placeholder={t("settings.networkLibrary.share")}
              onChange={(e) => setForm({ ...form, share: e.target.value })}
            />
          )}
          <input
            className={inputClass}
            value={form.root}
            spellCheck={false}
            aria-label={t("settings.networkLibrary.folder")}
            placeholder={t("settings.networkLibrary.folder")}
            onChange={(e) => setForm({ ...form, root: e.target.value })}
          />
          <input
            className={inputClass}
            value={form.username ?? ""}
            autoComplete="off"
            spellCheck={false}
            aria-label={t("settings.networkLibrary.username")}
            placeholder={t("settings.networkLibrary.username")}
            onChange={(e) => setForm({ ...form, username: e.target.value })}
          />
          <input
            className={inputClass}
            type="password"
            value={form.password ?? ""}
            autoComplete="off"
            aria-label={t("settings.networkLibrary.password")}
            placeholder={t("settings.networkLibrary.password")}
            onChange={(e) => setForm({ ...form, password: e.target.value })}
          />
          <div className="text-xs settings-description">
            {t("settings.networkLibrary.passwordNotice")}
          </div>
          {plainHttp && (
            <label className="flex items-start gap-2 text-xs text-amber-600 dark:text-amber-400">
              <input
                type="checkbox"
                className="mt-0.5"
                checked={form.allowPlainHttp}
                onChange={(e) =>
                  setForm({ ...form, allowPlainHttp: e.target.checked })
                }
              />
              {t("settings.networkLibrary.plainHttpWarning")}
            </label>
          )}
          {outcome && (
            <div
              role="status"
              className={`text-xs ${
                outcome.ok
                  ? "text-emerald-600 dark:text-emerald-400"
                  : "text-amber-600 dark:text-amber-400"
              }`}
            >
              {outcome.ok
                ? t("settings.networkLibrary.testOk", {
                    count: outcome.entries ?? 0,
                  })
                : failure(outcome)}
              {!outcome.ok && outcome.detail && (
                <div className="font-mono break-all opacity-80">
                  {outcome.detail}
                </div>
              )}
            </div>
          )}
          <div className="flex items-center gap-2">
            <button
              type="button"
              className={buttonClass}
              disabled={busy || !form.endpoint.trim()}
              onClick={() => void onTest()}
            >
              {t("settings.networkLibrary.test")}
            </button>
            <button
              type="button"
              className={buttonClass}
              disabled={busy || !form.endpoint.trim()}
              onClick={() => void onAdd()}
            >
              {t("settings.networkLibrary.addConfirm")}
            </button>
            <button
              type="button"
              className={buttonClass}
              onClick={() => {
                setForm(null);
                setOutcome(null);
              }}
            >
              {t("settings.networkLibrary.cancel")}
            </button>
          </div>
        </div>
      )}

      {error && (
        <div role="alert" className="ml-9 text-xs text-red-500 break-words">
          {error}
        </div>
      )}

      {sources.length > 0 && (
        <div className="ml-9 flex flex-wrap items-center gap-3 text-xs settings-description">
          <span>
            {t("settings.networkLibrary.cacheUsed", {
              size: cacheSize,
              count: cache?.entries ?? 0,
            })}
          </span>
          <label className="flex items-center gap-2">
            {t("settings.networkLibrary.cacheLimit")}
            <input
              type="number"
              min={MIN_CACHE_GB}
              max={MAX_CACHE_GB}
              className="w-20 px-2 py-1 rounded-lg bg-zinc-100 dark:bg-zinc-800 text-zinc-900 dark:text-white"
              key={cacheLimit.value}
              defaultValue={cacheLimit.value}
              onBlur={(e) => void onCacheLimit(e.target.value)}
            />
          </label>
          <button
            type="button"
            className={buttonClass}
            disabled={busy}
            onClick={() => void onClearCache()}
          >
            {t("settings.networkLibrary.cacheClear")}
          </button>
        </div>
      )}
    </div>
  );
}

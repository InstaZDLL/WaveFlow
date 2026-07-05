import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Database, HardDriveDownload, Trash2, Loader2 } from "lucide-react";

import {
  radioCatalogueStatus,
  downloadRadioCatalogue,
  clearRadioCatalogue,
  setRadioCatalogueLocalFirst,
  type RadioCatalogueStatus,
  type RadioCatalogueProgress,
} from "../../../lib/tauri/webRadioCatalogue";
import { getOfflineMode } from "../../../lib/tauri/offline";
import { ToggleSwitch } from "../../common/ToggleSwitch";

interface RadioCatalogueCardProps {
  language: string;
}

/**
 * Offline Web Radio catalogue card (Settings → Data). Downloads the
 * radio-browser station directory into the local FTS-indexed app.db table so
 * the Web Radio view can browse + search ~35k stations without network. The
 * `localFirst` toggle makes the view prefer the local snapshot even online.
 */
export function RadioCatalogueCard({ language }: RadioCatalogueCardProps) {
  const { t } = useTranslation();
  const [status, setStatus] = useState<RadioCatalogueStatus | null>(null);
  const [offline, setOffline] = useState(false);
  const [downloading, setDownloading] = useState(false);
  const [progress, setProgress] = useState<RadioCatalogueProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Drop a stale download's late progress event after another action reset
  // the row (clear / a second download).
  const downloadSeqRef = useRef(0);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [s, o] = await Promise.all([
          radioCatalogueStatus(),
          getOfflineMode(),
        ]);
        if (cancelled) return;
        setStatus(s);
        setOffline(o);
      } catch (err) {
        if (cancelled) return;
        console.error("[RadioCatalogueCard] load failed", err);
        // Fall back to an empty status so the card still renders (with the
        // error) instead of vanishing on the `if (!status) return null`
        // guard — the user can then read the message and retry.
        setStatus({ count: 0, lastSyncedAt: null, localFirst: false });
        setError(t("settings.radioCatalogue.errors.loadFailed"));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [t]);

  // Progress events fire on the backend's emit channel — subscribe once.
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    listen<RadioCatalogueProgress>("radio-catalogue:progress", (e) => {
      setProgress(e.payload);
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) =>
        console.error("[RadioCatalogueCard] progress listen failed", err),
      );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const handleDownload = async () => {
    const seq = ++downloadSeqRef.current;
    setDownloading(true);
    setError(null);
    setProgress(null);
    try {
      const count = await downloadRadioCatalogue();
      if (downloadSeqRef.current !== seq) return;
      const fresh = await radioCatalogueStatus();
      setStatus({ ...fresh, count });
    } catch (err) {
      if (downloadSeqRef.current !== seq) return;
      console.error("[RadioCatalogueCard] download failed", err);
      setError(t("settings.radioCatalogue.errors.downloadFailed"));
    } finally {
      if (downloadSeqRef.current === seq) {
        setDownloading(false);
        setProgress(null);
      }
    }
  };

  const handleClear = async () => {
    // Invalidate any in-flight download's late callbacks.
    downloadSeqRef.current++;
    try {
      await clearRadioCatalogue();
      const fresh = await radioCatalogueStatus();
      setStatus(fresh);
      setError(null);
    } catch (err) {
      console.error("[RadioCatalogueCard] clear failed", err);
      setError(t("settings.radioCatalogue.errors.clearFailed"));
    }
  };

  const handleToggleLocalFirst = async () => {
    if (!status) return;
    const next = !status.localFirst;
    setStatus({ ...status, localFirst: next }); // optimistic
    try {
      await setRadioCatalogueLocalFirst(next);
    } catch (err) {
      console.error("[RadioCatalogueCard] set local-first failed", err);
      setStatus({ ...status, localFirst: !next }); // rollback
    }
  };

  if (!status) return null;

  const hasCatalogue = status.count > 0;
  const lastSync =
    status.lastSyncedAt && status.lastSyncedAt > 0
      ? new Date(status.lastSyncedAt).toLocaleString(language)
      : t("settings.radioCatalogue.neverSynced");

  // Insert-phase percentage (download phase reports total 0 → indeterminate).
  const pct =
    progress && progress.phase === "insert" && progress.total > 0
      ? Math.min(100, Math.round((progress.current / progress.total) * 100))
      : null;

  return (
    <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
      <div className="flex items-center justify-between gap-4">
        <div className="flex items-center space-x-4 min-w-0">
          <Database
            size={20}
            className="text-zinc-400 shrink-0"
            aria-hidden="true"
          />
          <div className="min-w-0">
            <div className="text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.radioCatalogue.title")}
            </div>
            <div className="text-xs text-zinc-400">
              {hasCatalogue
                ? t("settings.radioCatalogue.stored", {
                    count: status.count,
                    date: lastSync,
                  })
                : t("settings.radioCatalogue.subtitle")}
            </div>
          </div>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          {hasCatalogue && !downloading && (
            <button
              type="button"
              onClick={handleClear}
              aria-label={t("settings.radioCatalogue.clear")}
              title={t("settings.radioCatalogue.clear")}
              className="p-2 rounded-lg text-zinc-400 hover:text-red-500 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
            >
              <Trash2 size={16} />
            </button>
          )}
          <button
            type="button"
            onClick={handleDownload}
            disabled={downloading || offline}
            title={
              offline ? t("settings.radioCatalogue.offlineHint") : undefined
            }
            className="inline-flex items-center gap-2 px-3 py-1.5 rounded-lg bg-emerald-500 text-white text-sm font-medium hover:bg-emerald-600 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {downloading ? (
              <Loader2 size={15} className="animate-spin" />
            ) : (
              <HardDriveDownload size={15} />
            )}
            <span>
              {downloading
                ? pct != null
                  ? t("settings.radioCatalogue.downloadingPct", { pct })
                  : t("settings.radioCatalogue.downloading")
                : hasCatalogue
                  ? t("settings.radioCatalogue.refresh")
                  : t("settings.radioCatalogue.download")}
            </span>
          </button>
        </div>
      </div>

      {offline && !downloading && (
        <p className="mt-3 ml-9 text-xs text-amber-600 dark:text-amber-400">
          {t("settings.radioCatalogue.offlineHint")}
        </p>
      )}

      {error && (
        <p className="mt-3 ml-9 text-xs text-red-500" role="alert">
          {error}
        </p>
      )}

      {/* Local-first toggle — only meaningful once a catalogue exists. */}
      {hasCatalogue && (
        <div className="mt-4 ml-9 flex items-center justify-between gap-4">
          <div className="min-w-0">
            <div className="text-sm text-zinc-600 dark:text-zinc-300">
              {t("settings.radioCatalogue.localFirst")}
            </div>
            <div className="text-xs text-zinc-400">
              {t("settings.radioCatalogue.localFirstHint")}
            </div>
          </div>
          <ToggleSwitch
            enabled={status.localFirst}
            onToggle={handleToggleLocalFirst}
            label={t("settings.radioCatalogue.localFirst")}
          />
        </div>
      )}
    </div>
  );
}

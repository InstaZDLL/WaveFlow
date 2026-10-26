import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { HardDrive, Loader2, RefreshCw } from "lucide-react";

import {
  announceStorageChanged,
  diskUsage,
  STORAGE_CHANGED_EVENT,
  type DiskUsage,
  type DiskUsageEntry,
} from "../../../lib/tauri/storage";
import { clearMotionCache } from "../../../lib/tauri/plugins";
import { clearCanvasCache } from "../../../lib/tauri/canvas";
import {
  remoteClearArtworkCache,
  remoteClearStreamCache,
} from "../../../lib/tauri/remoteServer";
import { formatBytes } from "../../../lib/format";
import { invalidateAllMotionArtwork } from "../../../hooks/useAlbumMotionArtwork";
import { invalidateAllTrackCanvas } from "../../../hooks/useTrackCanvas";

/**
 * How each rebuildable item is emptied: through the command its own card
 * already used, and with the same follow-up — the motion and Canvas
 * lookups remember local paths, which an emptied cache no longer holds.
 */
const EMPTY: Partial<Record<DiskUsageEntry["id"], () => Promise<unknown>>> = {
  motionCache: async () => {
    await clearMotionCache();
    invalidateAllMotionArtwork();
  },
  canvasCache: async () => {
    await clearCanvasCache();
    invalidateAllTrackCanvas();
  },
  remoteArtwork: remoteClearArtworkCache,
  remoteStream: remoteClearStreamCache,
};

/**
 * Settings → Storage: what WaveFlow keeps on this computer, item by item.
 *
 * Split in two on purpose. What rebuilds itself can be emptied from here,
 * since nothing is lost; what does not is only shown, because each of
 * those has its own card where the consequence of removing it is
 * explained (backups, offline copies, the clips set by hand).
 *
 * Measured when the page opens and on demand — a walk of the caches, so
 * the card says it is measuring rather than showing a figure it does not
 * have yet.
 */
export function DiskUsageCard({ language }: { language: string }) {
  const { t } = useTranslation();
  const [usage, setUsage] = useState<DiskUsage | null>(null);
  const [measuring, setMeasuring] = useState(true);
  const [failed, setFailed] = useState(false);
  const [confirming, setConfirming] = useState<DiskUsageEntry["id"] | null>(
    null,
  );
  const [emptying, setEmptying] = useState<DiskUsageEntry["id"] | null>(null);
  const [emptyFailed, setEmptyFailed] = useState<DiskUsageEntry["id"] | null>(
    null,
  );

  const measure = useCallback(async () => {
    setMeasuring(true);
    setFailed(false);
    try {
      setUsage(await diskUsage());
    } catch (err) {
      console.error("[DiskUsageCard] measure failed", err);
      setFailed(true);
    } finally {
      setMeasuring(false);
    }
  }, []);

  // The first measure: `measuring` already starts true, so nothing is set
  // before the answer arrives.
  useEffect(() => {
    let cancelled = false;
    diskUsage().then(
      (next) => {
        if (cancelled) return;
        setUsage(next);
        setMeasuring(false);
      },
      (err) => {
        if (cancelled) return;
        console.error("[DiskUsageCard] measure failed", err);
        setFailed(true);
        setMeasuring(false);
      },
    );
    return () => {
      cancelled = true;
    };
  }, []);

  // Emptied here or from another card on the page (the media caches
  // card): every figure shown measures again.
  useEffect(() => {
    const onChanged = () => void measure();
    window.addEventListener(STORAGE_CHANGED_EVENT, onChanged);
    return () => window.removeEventListener(STORAGE_CHANGED_EVENT, onChanged);
  }, [measure]);

  const empty = useCallback(
    async (id: DiskUsageEntry["id"]) => {
      const run = EMPTY[id];
      if (!run) return;
      if (confirming !== id) {
        setConfirming(id);
        return;
      }
      setConfirming(null);
      setEmptying(id);
      setEmptyFailed(null);
      try {
        await run();
      } catch (err) {
        // A file in use — the stream of the server track playing now, on
        // Windows — keeps the size where it was; say so rather than
        // re-measure the same figure in silence.
        console.error("[DiskUsageCard] empty failed", err);
        setEmptyFailed(id);
      } finally {
        setEmptying(null);
      }
      announceStorageChanged();
    },
    [confirming],
  );

  const rebuildable = usage?.entries.filter((e) => e.rebuildable) ?? [];
  const kept = usage?.entries.filter((e) => !e.rebuildable) ?? [];

  const row = (entry: DiskUsageEntry) => (
    <li
      key={entry.id}
      className="flex items-center justify-between gap-4 py-2.5"
    >
      <div className="min-w-0">
        <div className="text-sm text-zinc-800 dark:text-zinc-200">
          {t(`settings.diskUsage.items.${entry.id}.label`)}
        </div>
        <div className="text-xs settings-description">
          {t(`settings.diskUsage.items.${entry.id}.description`, {
            section: t("settings.organization.groups.cleanup"),
          })}
        </div>
        {emptyFailed === entry.id && (
          <div
            role="alert"
            className="mt-1 text-xs text-amber-700 dark:text-amber-300"
          >
            {t("settings.diskUsage.emptyFailed")}
          </div>
        )}
      </div>
      <div className="flex shrink-0 items-center gap-3">
        <span className="text-sm tabular-nums text-zinc-600 dark:text-zinc-300">
          {formatBytes(entry.bytes, language)}
        </span>
        {entry.rebuildable && EMPTY[entry.id] && (
          <button
            type="button"
            onClick={() => void empty(entry.id)}
            disabled={entry.bytes === 0 || emptying !== null || measuring}
            aria-label={t(
              confirming === entry.id
                ? "settings.diskUsage.confirmEmptyLabel"
                : "settings.diskUsage.emptyLabel",
              { item: t(`settings.diskUsage.items.${entry.id}.label`) },
            )}
            className="min-w-20 rounded-lg border border-zinc-200 px-3 py-1.5 text-xs font-medium text-zinc-700 hover:bg-zinc-50 disabled:cursor-not-allowed disabled:opacity-50 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors"
          >
            {emptying === entry.id ? (
              <Loader2
                size={12}
                className="mx-auto animate-spin"
                aria-hidden="true"
              />
            ) : confirming === entry.id ? (
              t("settings.diskUsage.confirmEmpty")
            ) : (
              t("settings.diskUsage.empty")
            )}
          </button>
        )}
      </div>
    </li>
  );

  return (
    <div className="settings-block">
      <div className="flex items-center justify-between gap-4">
        <div className="flex min-w-0 items-center space-x-4">
          <HardDrive
            size={16}
            className="settings-row-icon"
            aria-hidden="true"
          />
          <div className="min-w-0 text-sm text-zinc-900 dark:text-white">
            {measuring && !usage ? (
              <span className="flex items-center gap-2 text-zinc-500 dark:text-zinc-400">
                <Loader2
                  size={14}
                  className="animate-spin"
                  aria-hidden="true"
                />
                {t("settings.diskUsage.measuring")}
              </span>
            ) : failed ? (
              <span role="alert" className="text-amber-700 dark:text-amber-300">
                {t("settings.diskUsage.failed")}
              </span>
            ) : (
              usage &&
              t("settings.diskUsage.total", {
                size: formatBytes(usage.totalBytes, language),
              })
            )}
          </div>
        </div>
        <button
          type="button"
          onClick={() => void measure()}
          disabled={measuring || emptying !== null}
          className="flex shrink-0 items-center gap-1.5 text-xs font-medium text-zinc-500 hover:text-zinc-800 disabled:opacity-50 dark:hover:text-zinc-200 transition-colors"
        >
          <RefreshCw
            size={12}
            className={measuring ? "animate-spin" : undefined}
            aria-hidden="true"
          />
          <span>{t("settings.diskUsage.refresh")}</span>
        </button>
      </div>
      {usage && (
        <div className="mt-4 space-y-4 ps-8">
          {rebuildable.length > 0 && (
            <section>
              <h4 className="text-xs font-semibold text-zinc-700 dark:text-zinc-200">
                {t("settings.diskUsage.rebuildableTitle")}
              </h4>
              <p className="text-xs settings-description">
                {t("settings.diskUsage.rebuildableHint")}
              </p>
              <ul className="divide-y divide-zinc-100 dark:divide-zinc-700/50">
                {rebuildable.map(row)}
              </ul>
            </section>
          )}
          <section>
            <h4 className="text-xs font-semibold text-zinc-700 dark:text-zinc-200">
              {t("settings.diskUsage.keptTitle")}
            </h4>
            <p className="text-xs settings-description">
              {t("settings.diskUsage.keptHint")}
            </p>
            <ul className="divide-y divide-zinc-100 dark:divide-zinc-700/50">
              {kept.map(row)}
            </ul>
          </section>
        </div>
      )}
    </div>
  );
}

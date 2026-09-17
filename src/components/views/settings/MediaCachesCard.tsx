import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Film, Trash2 } from "lucide-react";

import {
  getMotionCacheInfo,
  setMotionCacheEnabled,
  clearMotionCache,
} from "../../../lib/tauri/plugins";
import {
  getCanvasCacheInfo,
  setCanvasCacheEnabled,
  clearCanvasCache,
} from "../../../lib/tauri/canvas";
import { formatBytes } from "../../../lib/format";
import { invalidateAllMotionArtwork } from "../../../hooks/useAlbumMotionArtwork";
import { invalidateAllTrackCanvas } from "../../../hooks/useTrackCanvas";

/**
 * Clearing deletes the files, and with the cache on the backend had answered
 * with their LOCAL paths — which the lookup hooks still remember. Without the
 * invalidation, every cover or Canvas already resolved would keep pointing at
 * a deleted file until the app restarts. The whole cache is gone, so the whole
 * frontend cache is dropped, not one album's or one track's.
 *
 * Only after the clear succeeds: a failed clear leaves the files — and so the
 * remembered paths — valid.
 */
async function clearMotionCacheAndForget(): Promise<void> {
  await clearMotionCache();
  invalidateAllMotionArtwork();
}

async function clearCanvasCacheAndForget(): Promise<void> {
  await clearCanvasCache();
  invalidateAllTrackCanvas();
}

/**
 * Settings → Data row for the two app-wide video caches plugins fill:
 * animated album covers and per-track Canvas clips.
 *
 * Here and not under a plugin, because neither cache belongs to one. Both
 * are single LRUs shared by every plugin that produces that kind of video,
 * and `CacheLocationCard` just above already decides which drive they live
 * on. Rendered per plugin, the same switch and the same footprint appeared
 * under each one, and a plugin's world was the only thing deciding whether
 * it got them — which put a motion-cover switch under a lyrics plugin.
 *
 * Always shown, rather than only while a producing plugin is installed:
 * nothing on the host says which plugins produce what, and a cache that
 * still holds files after its plugin is uninstalled is exactly the one a
 * user wants to be able to clear.
 */
export function MediaCachesCard({ language }: { language: string }) {
  return (
    <section className="py-5 px-4 rounded-xl">
      <div className="flex items-start space-x-4">
        <Film
          size={20}
          className="text-zinc-400 mt-0.5 shrink-0"
          aria-hidden="true"
        />
        <div className="min-w-0 flex-1 space-y-5">
          <LocalCacheOption
            i18nPrefix="settings.motionArtwork"
            toggleId="motion-cache-toggle"
            language={language}
            getInfo={getMotionCacheInfo}
            setEnabled={setMotionCacheEnabled}
            clear={clearMotionCacheAndForget}
          />
          <LocalCacheOption
            i18nPrefix="settings.canvasCache"
            toggleId="canvas-cache-toggle"
            language={language}
            getInfo={getCanvasCacheInfo}
            setEnabled={setCanvasCacheEnabled}
            clear={clearCanvasCacheAndForget}
          />
        </div>
      </div>
    </section>
  );
}

/** One opt-in local mp4 cache (motion artwork or Canvas): identical host-side
 *  shape (toggle + on-disk footprint + clear), only the backend commands + the
 *  i18n prefix differ. */
interface LocalCacheProps {
  i18nPrefix: "settings.motionArtwork" | "settings.canvasCache";
  toggleId: string;
  /** UI language, so the footprint reads "9,4 MB" where that is the norm. */
  language: string;
  getInfo: () => Promise<{
    enabled: boolean;
    sizeBytes: number;
    fileCount: number;
  }>;
  setEnabled: (enabled: boolean) => Promise<void>;
  clear: () => Promise<void>;
}

function LocalCacheOption({
  i18nPrefix,
  toggleId,
  language,
  getInfo,
  setEnabled: persistEnabled,
  clear,
}: LocalCacheProps) {
  const { t } = useTranslation();
  const [enabled, setEnabled] = useState(false);
  const [sizeBytes, setSizeBytes] = useState(0);
  const [fileCount, setFileCount] = useState(0);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [busy, setBusy] = useState(false);
  const [confirmingClear, setConfirmingClear] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getInfo().then(
      (info) => {
        if (cancelled) return;
        setEnabled(info.enabled);
        setSizeBytes(info.sizeBytes);
        setFileCount(info.fileCount);
        setLoading(false);
      },
      (e) => {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : String(e));
        setLoading(false);
      },
    );
    return () => {
      cancelled = true;
    };
  }, [getInfo]);

  const onToggle = useCallback(async () => {
    // Serialise against BOTH the toggle write and the clear op — ignore clicks
    // while either is in flight so a set + clear (or two sets) can't overlap.
    if (saving || busy) return;
    const next = !enabled;
    setSaving(true);
    setEnabled(next); // optimistic
    setError(null);
    try {
      await persistEnabled(next);
      // No invalidation here, unlike after a clear, and on purpose. Turning
      // the cache OFF deletes nothing, so every remembered local path still
      // names a file that exists; turning it ON leaves remembered remote
      // URLs, which still stream. Either way the source on screen stays
      // playable, and invalidating would only reload every mounted video —
      // a visible flicker and a fresh download or re-stream — to fix
      // nothing. The new mode applies from the next lookup. A clear is the
      // opposite case: it deletes the very files those paths name.
    } catch (e) {
      setEnabled(!next); // revert
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }, [enabled, saving, busy, persistEnabled]);

  const onClear = useCallback(async () => {
    // Don't start a clear while a toggle write (or another clear) is pending.
    if (saving || busy) return;
    if (!confirmingClear) {
      setConfirmingClear(true);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await clear();
      const info = await getInfo();
      setSizeBytes(info.sizeBytes);
      setFileCount(info.fileCount);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
      setConfirmingClear(false);
    }
  }, [confirmingClear, saving, busy, getInfo, clear]);

  return (
    <div>
      {error && (
        <div
          role="alert"
          className="mb-2 px-2 py-1.5 bg-red-50 dark:bg-red-950/30 text-xs text-red-700 dark:text-red-300 rounded"
        >
          {error}
        </div>
      )}

      <div className="flex items-center justify-between gap-4">
        <div className="min-w-0">
          <label
            htmlFor={toggleId}
            className="text-sm text-zinc-700 dark:text-zinc-200 select-none block"
          >
            {t(`${i18nPrefix}.cacheLabel`)}
          </label>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">
            {t(`${i18nPrefix}.subtitle`)}
          </p>
        </div>
        <button
          id={toggleId}
          type="button"
          role="switch"
          aria-checked={enabled}
          disabled={loading || saving || busy}
          onClick={onToggle}
          className={`relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 focus-visible:ring-offset-2 disabled:opacity-50 ${
            enabled ? "bg-emerald-500" : "bg-zinc-300 dark:bg-zinc-700"
          }`}
        >
          <span
            className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
              enabled ? "translate-x-6" : "translate-x-1"
            }`}
          />
        </button>
      </div>

      <div className="mt-2 flex items-center justify-between gap-4">
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          {t(`${i18nPrefix}.usage`, {
            size: formatBytes(sizeBytes, language),
            files: fileCount,
          })}
        </p>
        <button
          type="button"
          onClick={onClear}
          disabled={saving || busy || (fileCount === 0 && !confirmingClear)}
          className="flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium text-red-600 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-950/30 rounded disabled:opacity-40 disabled:hover:bg-transparent focus:outline-none focus-visible:ring-2 focus-visible:ring-red-500"
        >
          <Trash2 size={14} aria-hidden="true" />
          {confirmingClear
            ? t(`${i18nPrefix}.clearConfirm`)
            : t(`${i18nPrefix}.clear`)}
        </button>
      </div>
    </div>
  );
}

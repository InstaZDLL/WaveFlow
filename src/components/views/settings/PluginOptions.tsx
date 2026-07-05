import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Trash2 } from "lucide-react";

import {
  getMotionCacheInfo,
  setMotionCacheEnabled,
  clearMotionCache,
  isMetadataPlugin,
  type PluginInfo,
} from "../../../lib/tauri/plugins";

/** Human-readable byte size (locale-agnostic, 1 decimal for MB/GB). */
function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${Math.round(kb)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  return `${(mb / 1024).toFixed(2)} GB`;
}

/**
 * Inline per-plugin options panel, revealed under a plugin row when its gear
 * is clicked (see PluginsCard). Keeps options scoped to the plugin the user
 * opened instead of stacking a card per plugin — the list stays short no
 * matter how many plugins are installed.
 *
 * For metadata-world plugins it renders the host-provided motion-artwork
 * local-cache control (toggle + footprint + clear). The motion cache is an
 * app-wide WaveFlow capability, surfaced here because it caches what these
 * plugins produce.
 */
export function PluginOptions({ plugin }: { plugin: PluginInfo }) {
  return (
    <div className="mt-3 pt-3 border-t border-zinc-200 dark:border-zinc-800 space-y-3">
      {isMetadataPlugin(plugin) && <MotionCacheOption />}
    </div>
  );
}

function MotionCacheOption() {
  const { t } = useTranslation();
  const [enabled, setEnabled] = useState(false);
  const [sizeBytes, setSizeBytes] = useState(0);
  const [fileCount, setFileCount] = useState(0);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [confirmingClear, setConfirmingClear] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getMotionCacheInfo().then(
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
  }, []);

  const onToggle = useCallback(async () => {
    const next = !enabled;
    setEnabled(next); // optimistic
    setError(null);
    try {
      await setMotionCacheEnabled(next);
    } catch (e) {
      setEnabled(!next); // revert
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [enabled]);

  const onClear = useCallback(async () => {
    if (!confirmingClear) {
      setConfirmingClear(true);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await clearMotionCache();
      const info = await getMotionCacheInfo();
      setSizeBytes(info.sizeBytes);
      setFileCount(info.fileCount);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
      setConfirmingClear(false);
    }
  }, [confirmingClear]);

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
            htmlFor="motion-cache-toggle"
            className="text-sm text-zinc-700 dark:text-zinc-200 select-none block"
          >
            {t("settings.motionArtwork.cacheLabel")}
          </label>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">
            {t("settings.motionArtwork.subtitle")}
          </p>
        </div>
        <button
          id="motion-cache-toggle"
          type="button"
          role="switch"
          aria-checked={enabled}
          disabled={loading}
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
          {t("settings.motionArtwork.usage", {
            size: formatBytes(sizeBytes),
            files: fileCount,
          })}
        </p>
        <button
          type="button"
          onClick={onClear}
          disabled={busy || (fileCount === 0 && !confirmingClear)}
          className="flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium text-red-600 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-950/30 rounded disabled:opacity-40 disabled:hover:bg-transparent focus:outline-none focus-visible:ring-2 focus-visible:ring-red-500"
        >
          <Trash2 size={14} aria-hidden="true" />
          {confirmingClear
            ? t("settings.motionArtwork.clearConfirm")
            : t("settings.motionArtwork.clear")}
        </button>
      </div>
    </div>
  );
}

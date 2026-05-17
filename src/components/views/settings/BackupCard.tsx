import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Archive, FolderOpen, Play } from "lucide-react";

import {
  getBackupConfig,
  runBackupNow,
  setBackupConfig,
  type BackupConfig,
} from "../../../lib/tauri/backup";
import { pickFolder } from "../../../lib/tauri/dialog";
import { ToggleSwitch } from "../../common/ToggleSwitch";

/// Day presets the user can pick from. Matches what backup tools like
/// Time Machine / Backblaze offer; users wanting an off-grid cadence
/// can edit the SQLite row directly.
const INTERVAL_OPTIONS = [1, 3, 7, 14, 30];
const RETENTION_OPTIONS = [3, 5, 10, 20];

interface BackupCardProps {
  language: string;
}

/**
 * Auto-backup configuration card. Lives in Settings → Stockage right
 * after the manual profile export/import row so the two related
 * features cluster together.
 *
 * State management is intentionally local — the backup config is
 * single-card and never read elsewhere in the UI, so a global context
 * would just be ceremony.
 */
export function BackupCard({ language }: BackupCardProps) {
  const { t } = useTranslation();
  const [config, setConfig] = useState<BackupConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const [running, setRunning] = useState(false);
  const [status, setStatus] = useState<{
    kind: "ok" | "error";
    message: string;
  } | null>(null);

  useEffect(() => {
    getBackupConfig()
      .then(setConfig)
      .catch((err) => {
        console.error("[BackupCard] get_backup_config failed", err);
        setStatus({
          kind: "error",
          message: t("settings.backup.errors.loadFailed"),
        });
      });
  }, [t]);

  // Single source of truth for "user changed X → push the whole
  // config down to the backend". Without this the toggle + interval +
  // folder + retention would each need their own debounced setter.
  const persist = async (patch: Partial<BackupConfig>) => {
    if (!config) return;
    const next: BackupConfig = { ...config, ...patch };
    setConfig(next);
    setSaving(true);
    try {
      await setBackupConfig({
        enabled: next.enabled,
        interval_days: next.interval_days,
        folder: next.folder,
        retention: next.retention,
        include_metadata_artwork: next.include_metadata_artwork,
      });
      setStatus(null);
    } catch (err) {
      console.error("[BackupCard] set_backup_config failed", err);
      setStatus({
        kind: "error",
        message: t("settings.backup.errors.saveFailed"),
      });
    } finally {
      setSaving(false);
    }
  };

  const handlePickFolder = async () => {
    const folder = await pickFolder(t("settings.backup.pickFolderTitle"));
    if (!folder) return;
    persist({ folder });
  };

  const handleRunNow = async () => {
    setRunning(true);
    setStatus(null);
    try {
      const paths = await runBackupNow();
      setStatus({
        kind: "ok",
        message: t("settings.backup.runOk", { count: paths.length }),
      });
      // Refresh config so `last_run_at` updates.
      const fresh = await getBackupConfig();
      setConfig(fresh);
    } catch (err) {
      console.error("[BackupCard] run_backup_now failed", err);
      setStatus({
        kind: "error",
        message: t("settings.backup.errors.runFailed"),
      });
    } finally {
      setRunning(false);
    }
  };

  if (!config) {
    return null;
  }

  const effectiveFolder = config.folder || config.default_folder;
  const lastRun =
    config.last_run_at > 0
      ? new Date(config.last_run_at).toLocaleString(language)
      : t("settings.backup.neverRun");

  return (
    <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
      <div className="flex items-center justify-between gap-4">
        <div className="flex items-center space-x-4 min-w-0">
          <Archive
            size={20}
            className="text-zinc-400 shrink-0"
            aria-hidden="true"
          />
          <div className="min-w-0">
            <div className="text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.backup.title")}
            </div>
            <div className="text-xs text-zinc-400">
              {t("settings.backup.subtitle")}
            </div>
          </div>
        </div>
        <ToggleSwitch
          enabled={config.enabled}
          onToggle={() => persist({ enabled: !config.enabled })}
          label={t("settings.backup.title")}
        />
      </div>

      {config.enabled && (
        <div className="mt-4 ml-9 space-y-3">
          {/* Interval picker */}
          <div className="flex items-center justify-between gap-4">
            <label
              htmlFor="backup-interval"
              className="text-sm text-zinc-600 dark:text-zinc-300"
            >
              {t("settings.backup.intervalLabel")}
            </label>
            <select
              id="backup-interval"
              value={config.interval_days}
              onChange={(e) =>
                persist({ interval_days: parseInt(e.target.value, 10) })
              }
              disabled={saving}
              className="px-3 py-1.5 rounded-lg border border-zinc-200 bg-white text-sm text-zinc-700 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-200 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50"
            >
              {INTERVAL_OPTIONS.map((days) => (
                <option key={days} value={days}>
                  {t("settings.backup.intervalDays", { count: days })}
                </option>
              ))}
            </select>
          </div>

          {/* Retention picker */}
          <div className="flex items-center justify-between gap-4">
            <label
              htmlFor="backup-retention"
              className="text-sm text-zinc-600 dark:text-zinc-300"
            >
              {t("settings.backup.retentionLabel")}
            </label>
            <select
              id="backup-retention"
              value={config.retention}
              onChange={(e) =>
                persist({ retention: parseInt(e.target.value, 10) })
              }
              disabled={saving}
              className="px-3 py-1.5 rounded-lg border border-zinc-200 bg-white text-sm text-zinc-700 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-200 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50"
            >
              {RETENTION_OPTIONS.map((keep) => (
                <option key={keep} value={keep}>
                  {t("settings.backup.retentionKeep", { count: keep })}
                </option>
              ))}
            </select>
          </div>

          {/* Include shared Deezer artwork cache */}
          <div className="flex items-start justify-between gap-4">
            <label
              htmlFor="backup-include-meta"
              className="text-sm text-zinc-600 dark:text-zinc-300 cursor-pointer select-none flex-1"
            >
              <span className="block">
                {t("settings.backup.includeMetadataArtworkLabel")}
              </span>
              <span className="block text-xs text-zinc-400 mt-0.5">
                {t("settings.backup.includeMetadataArtworkHint")}
              </span>
            </label>
            <input
              id="backup-include-meta"
              type="checkbox"
              checked={config.include_metadata_artwork}
              onChange={(e) =>
                persist({ include_metadata_artwork: e.target.checked })
              }
              disabled={saving}
              className="mt-1 h-4 w-4 accent-emerald-500 cursor-pointer disabled:opacity-50"
            />
          </div>

          {/* Folder + actions */}
          <div className="flex items-center justify-between gap-4">
            <div className="flex items-center gap-2 min-w-0 text-sm text-zinc-600 dark:text-zinc-300">
              <FolderOpen
                size={14}
                className="shrink-0 text-zinc-400"
                aria-hidden="true"
              />
              <span
                className="font-mono text-xs truncate"
                title={effectiveFolder}
              >
                {effectiveFolder}
              </span>
            </div>
            <button
              type="button"
              onClick={handlePickFolder}
              disabled={saving}
              className="px-3 py-1.5 rounded-lg border border-zinc-200 bg-white text-xs font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50"
            >
              {t("settings.backup.changeFolder")}
            </button>
          </div>

          <div className="flex items-center justify-between gap-4 pt-1">
            <div className="text-xs text-zinc-400">
              {t("settings.backup.lastRun", { when: lastRun })}
            </div>
            <button
              type="button"
              onClick={handleRunNow}
              disabled={running}
              className="flex items-center space-x-2 px-3 py-1.5 rounded-lg border border-zinc-200 bg-white text-xs font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50"
            >
              <Play
                size={12}
                aria-hidden="true"
                className={running ? "animate-pulse" : ""}
              />
              <span>{t("settings.backup.runNow")}</span>
            </button>
          </div>
        </div>
      )}

      {status && (
        <div
          className={`mt-3 ml-9 text-xs ${
            status.kind === "ok"
              ? "text-emerald-600 dark:text-emerald-400"
              : "text-red-500"
          }`}
        >
          {status.message}
        </div>
      )}
    </div>
  );
}

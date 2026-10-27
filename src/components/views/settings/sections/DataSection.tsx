import type { Profile } from "../../../../lib/tauri/profile";
import { useTranslation } from "react-i18next";
import { BackupCard } from "../BackupCard";
import { CacheLocationCard } from "../CacheLocationCard";
import { ClipsInLibraryCard } from "../ClipsInLibraryCard";
import { DiskUsageCard } from "../DiskUsageCard";
import { MediaCachesCard } from "../MediaCachesCard";
import { SettingsCategoryHeader } from "../SettingsCategoryHeader";
import { SettingsGroup } from "../SettingsGroup";
import {
  Download,
  FolderOpen,
  ImageIcon,
  Loader2,
  RefreshCcw,
  Upload,
} from "lucide-react";

interface DataSectionProps {
  activeProfile: Profile | null;
  handleExportProfile: () => Promise<void>;
  handleImportProfile: () => Promise<void>;
  handleOpenDataFolder: () => Promise<void>;
  handlePruneCachedCovers: () => Promise<void>;
  handleRegenerateThumbnails: () => Promise<void>;
  isPruningCovers: boolean;
  isRegeneratingThumbs: boolean;
  profileIoBusy: "export" | "import" | null;
  profileIoStatus: { kind: "ok" | "fail"; message: string } | null;
  pruneCoversStatus: { ok: boolean; text: string } | null;
  targetGroup: string | null;
  thumbsStatus: string | null;
}

export function DataSection({
  activeProfile,
  handleExportProfile,
  handleImportProfile,
  handleOpenDataFolder,
  handlePruneCachedCovers,
  handleRegenerateThumbnails,
  isPruningCovers,
  isRegeneratingThumbs,
  profileIoBusy,
  profileIoStatus,
  pruneCoversStatus,
  targetGroup,
  thumbsStatus,
}: DataSectionProps) {
  const { i18n, t } = useTranslation();
  return (
    <section
      aria-labelledby="settings-heading-data"
      className="settings-category"
    >
      <SettingsCategoryHeader category="data" />
      <SettingsGroup id="diskUsage" reveal={targetGroup === "diskUsage"}>
        {/* Keyed by profile: the per-profile items (covers,
                    offline copies, server caches) are the active
                    profile's, and Settings stays mounted across a
                    switch. */}
        <DiskUsageCard
          key={activeProfile?.id ?? 0}
          language={i18n.resolvedLanguage ?? i18n.language}
        />
      </SettingsGroup>
      <SettingsGroup id="backups" reveal={targetGroup === "backups"}>
        <div className="settings-block">
          <div className="flex items-center justify-between gap-4">
            <div className="flex items-center space-x-4 min-w-0">
              <Download
                size={16}
                className="settings-row-icon"
                aria-hidden="true"
              />
              <div className="min-w-0">
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.profileIo.title")}
                </div>
                <div className="text-xs settings-description">
                  {t("settings.profileIo.subtitle")}
                </div>
              </div>
            </div>
            <div className="flex items-center space-x-2 shrink-0">
              <button
                type="button"
                onClick={handleExportProfile}
                disabled={profileIoBusy != null || !activeProfile}
                className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <Download
                  size={14}
                  aria-hidden="true"
                  className={profileIoBusy === "export" ? "animate-pulse" : ""}
                />
                <span>{t("settings.profileIo.export.action")}</span>
              </button>
              <button
                type="button"
                onClick={handleImportProfile}
                disabled={profileIoBusy != null}
                className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <Upload
                  size={14}
                  aria-hidden="true"
                  className={profileIoBusy === "import" ? "animate-pulse" : ""}
                />
                <span>{t("settings.profileIo.import.action")}</span>
              </button>
            </div>
          </div>
          {profileIoBusy && !profileIoStatus && (
            // An archive carries the artwork cache too, so a large
            // library takes minutes; greyed buttons alone read as
            // a hang.
            <div
              role="status"
              className="mt-2 ml-9 text-xs text-zinc-500 dark:text-zinc-400"
            >
              {t(`settings.profileIo.${profileIoBusy}.busy`)}
            </div>
          )}
          {profileIoStatus && (
            <div
              className={`mt-2 ml-9 text-xs ${
                profileIoStatus.kind === "ok"
                  ? "text-emerald-600 dark:text-emerald-400"
                  : "text-red-500"
              }`}
            >
              {profileIoStatus.message}
            </div>
          )}
        </div>
        <BackupCard language={i18n.resolvedLanguage ?? i18n.language} />
      </SettingsGroup>
      <SettingsGroup id="storage" reveal={targetGroup === "storage"}>
        <CacheLocationCard language={i18n.resolvedLanguage ?? i18n.language} />
        <MediaCachesCard language={i18n.resolvedLanguage ?? i18n.language} />
        <ClipsInLibraryCard />
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <FolderOpen
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.dataFolder.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.dataFolder.subtitle")}
              </div>
            </div>
          </div>
          <button
            type="button"
            onClick={handleOpenDataFolder}
            aria-label={t("settings.openDataFolder")}
            className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
          >
            <FolderOpen size={14} aria-hidden="true" />
            <span>{t("settings.dataFolder.action")}</span>
          </button>
        </div>
      </SettingsGroup>
      <SettingsGroup id="cleanup" advanced reveal={targetGroup === "cleanup"}>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4 flex-1 min-w-0">
            <ImageIcon
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div className="min-w-0">
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.regenerateThumbnails")}
              </div>
              {thumbsStatus ? (
                <div className="text-xs text-emerald-600 dark:text-emerald-400 mt-1 truncate">
                  {thumbsStatus}
                </div>
              ) : (
                <div className="text-xs settings-description">
                  {t("settings.regenerateThumbnailsSubtitle")}
                </div>
              )}
            </div>
          </div>
          <button
            type="button"
            onClick={handleRegenerateThumbnails}
            disabled={isRegeneratingThumbs}
            className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isRegeneratingThumbs ? (
              <Loader2 size={14} className="animate-spin" aria-hidden="true" />
            ) : (
              <RefreshCcw size={14} aria-hidden="true" />
            )}
            <span>{t("settings.regenerateThumbnailsAction")}</span>
          </button>
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4 flex-1 min-w-0">
            <ImageIcon
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div className="min-w-0">
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.pruneAlbumCovers")}
              </div>
              {pruneCoversStatus ? (
                <div
                  role="status"
                  aria-live="polite"
                  className={`text-xs mt-1 truncate ${
                    pruneCoversStatus.ok
                      ? "text-emerald-600 dark:text-emerald-400"
                      : "text-rose-600 dark:text-rose-400"
                  }`}
                >
                  {pruneCoversStatus.text}
                </div>
              ) : (
                <div className="text-xs settings-description">
                  {t("settings.pruneAlbumCoversSubtitle")}
                </div>
              )}
            </div>
          </div>
          <button
            type="button"
            onClick={handlePruneCachedCovers}
            disabled={isPruningCovers}
            className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isPruningCovers ? (
              <Loader2 size={14} className="animate-spin" aria-hidden="true" />
            ) : (
              <RefreshCcw size={14} aria-hidden="true" />
            )}
            <span>{t("settings.pruneAlbumCoversAction")}</span>
          </button>
        </div>
      </SettingsGroup>
    </section>
  );
}

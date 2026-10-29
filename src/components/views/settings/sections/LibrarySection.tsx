import type { Dispatch, SetStateAction } from "react";
import type { TaskSnapshot } from "../../../../lib/tauri/tasks";
import type { Library } from "../../../../lib/tauri/library";
import { useTranslation } from "react-i18next";
import { ArtistBioSourceCard } from "../ArtistBioSourceCard";
import { RadioCatalogueCard } from "../RadioCatalogueCard";
import { SettingsCategoryHeader } from "../SettingsCategoryHeader";
import { SettingsGroup } from "../SettingsGroup";
import { ToggleSwitch } from "../ToggleSwitch";
import {
  ArrowDownAZ,
  Copy,
  ImageIcon,
  Loader2,
  RefreshCcw,
  ScanLine,
  Sparkles,
  Square,
} from "lucide-react";

interface LibrarySectionProps {
  analysisTask: TaskSnapshot | undefined;
  analyzeFailed: number;
  artistFetchProgress: {
    current: number;
    total: number;
    artistName: string;
  } | null;
  autoAnalyze: boolean;
  coverProgress: { current: number; total: number; albumTitle: string } | null;
  coverResultMsg: string | null;
  handleAnalyzeLibrary: () => Promise<void>;
  handleCancelAnalyzeLibrary: () => Promise<void>;
  handleFetchMissingArtistPictures: () => Promise<void>;
  handleFetchMissingCovers: () => Promise<void>;
  handleRescan: () => Promise<void>;
  handleRescanLocalArtistImages: () => Promise<void>;
  handleToggleAutoAnalyze: () => void;
  handleToggleIgnoreArticles: () => void;
  handleToggleScanOnStart: () => void;
  ignoreArticles: boolean;
  isAnalyzingLib: boolean;
  isFetchingArtists: boolean;
  isFetchingCovers: boolean;
  isRescanning: boolean;
  isRescanningLocalArtists: boolean;
  libraries: Library[];
  localArtistRescanStatus: string | null;
  savingIgnoreArticles: boolean;
  scanOnStart: boolean;
  setIsDuplicatesOpen: Dispatch<SetStateAction<boolean>>;
  targetGroup: string | null;
}

export function LibrarySection({
  analysisTask,
  analyzeFailed,
  artistFetchProgress,
  autoAnalyze,
  coverProgress,
  coverResultMsg,
  handleAnalyzeLibrary,
  handleCancelAnalyzeLibrary,
  handleFetchMissingArtistPictures,
  handleFetchMissingCovers,
  handleRescan,
  handleRescanLocalArtistImages,
  handleToggleAutoAnalyze,
  handleToggleIgnoreArticles,
  handleToggleScanOnStart,
  ignoreArticles,
  isAnalyzingLib,
  isFetchingArtists,
  isFetchingCovers,
  isRescanning,
  isRescanningLocalArtists,
  libraries,
  localArtistRescanStatus,
  savingIgnoreArticles,
  scanOnStart,
  setIsDuplicatesOpen,
  targetGroup,
}: LibrarySectionProps) {
  const { i18n, t } = useTranslation();
  return (
    <section
      aria-labelledby="settings-heading-library"
      className="settings-category"
    >
      <SettingsCategoryHeader category="library" />
      <SettingsGroup id="scanning" reveal={targetGroup === "scanning"}>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <ScanLine
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.scanOnStart.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.scanOnStart.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={scanOnStart}
            onToggle={handleToggleScanOnStart}
            label={t("settings.scanOnStart.title")}
          />
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <ArrowDownAZ
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.sortIgnoreArticles.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.sortIgnoreArticles.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={ignoreArticles}
            onToggle={handleToggleIgnoreArticles}
            label={t("settings.sortIgnoreArticles.title")}
            disabled={savingIgnoreArticles}
          />
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <RefreshCcw
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.rescan.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.rescan.subtitle")}
              </div>
            </div>
          </div>
          <button
            type="button"
            onClick={handleRescan}
            disabled={isRescanning || libraries.length === 0}
            className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isRescanning ? (
              <Loader2 size={14} className="animate-spin" aria-hidden="true" />
            ) : (
              <RefreshCcw size={14} aria-hidden="true" />
            )}
            <span>{t("settings.rescan.action")}</span>
          </button>
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Copy size={16} className="settings-row-icon" aria-hidden="true" />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.duplicates.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.duplicates.subtitle")}
              </div>
            </div>
          </div>
          <button
            type="button"
            onClick={() => setIsDuplicatesOpen(true)}
            className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
          >
            <Copy size={14} aria-hidden="true" />
            <span>{t("settings.duplicates.action")}</span>
          </button>
        </div>
      </SettingsGroup>
      <SettingsGroup id="analysis" reveal={targetGroup === "analysis"}>
        <div className="settings-block">
          <div className="flex items-center justify-between">
            <div className="flex items-center space-x-4">
              <Sparkles
                size={16}
                className="settings-row-icon"
                aria-hidden="true"
              />
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.analyze.title")}
                </div>
                <div className="text-xs settings-description">
                  {t("settings.analyze.subtitle")}
                </div>
              </div>
            </div>
            {isAnalyzingLib ? (
              // Mid-run: swap the Analyze button out for a Stop
              // button so the user can recover from a long-running
              // sweep that's saturating their CPU (issue #286).
              // The backend `cancel_library_analysis` flips a flag
              // the worker loop checks at every track boundary —
              // the click resolves quickly but the loop only exits
              // after the current decode finishes (single track
              // delay is acceptable; aborting mid-decode would
              // require restructuring `analyze_file` for cancel
              // tokens, deferred).
              <button
                type="button"
                onClick={handleCancelAnalyzeLibrary}
                className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-rose-200 bg-rose-50 text-sm font-medium text-rose-700 hover:bg-rose-100 dark:border-rose-900/40 dark:bg-rose-950/30 dark:text-rose-300 dark:hover:bg-rose-950/50 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-rose-500"
              >
                <Square size={14} aria-hidden="true" />
                <span>{t("settings.analyze.cancel")}</span>
              </button>
            ) : (
              <button
                type="button"
                onClick={handleAnalyzeLibrary}
                disabled={libraries.length === 0}
                className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <Sparkles size={14} aria-hidden="true" />
                <span>{t("settings.analyze.action")}</span>
              </button>
            )}
          </div>
          {/* Auto-analyze toggle: when on, every scan that
                adds new tracks fires the analyzer in the
                background. Sits inside the same card so users
                see it as a related option rather than a
                disconnected setting. */}
          <div className="mt-3 ml-9 flex items-center justify-between">
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.analyze.autoTitle")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.analyze.autoSubtitle")}
              </div>
            </div>
            <ToggleSwitch
              enabled={autoAnalyze}
              onToggle={handleToggleAutoAnalyze}
              label={t("settings.analyze.autoTitle")}
            />
          </div>
          {/* Progress strip — shown while the sweep runs, whoever
                started it (this card, or the automatic analysis after a
                scan). */}
          {analysisTask && (
            <div className="mt-3 ml-9">
              <div className="flex justify-between text-[11px] text-zinc-500 mb-1">
                <span>
                  {analysisTask.total > 0
                    ? `${analysisTask.current} / ${analysisTask.total}`
                    : analysisTask.current}
                </span>
                {analyzeFailed > 0 && (
                  <span className="text-rose-500">
                    {t("settings.analyze.failed", {
                      count: analyzeFailed,
                    })}
                  </span>
                )}
              </div>
              <div className="h-1.5 rounded-full bg-zinc-200 dark:bg-zinc-700 overflow-hidden">
                {/* Until the sweep has counted its queue the total
                        is 0, "unknown": a pulsing third, as in the task
                        bar, rather than an empty bar that reads as stuck. */}
                <div
                  className={
                    analysisTask.total > 0
                      ? "h-full bg-emerald-500 transition-all duration-200"
                      : "h-full w-1/3 bg-emerald-500 opacity-70 motion-safe:animate-pulse"
                  }
                  style={
                    analysisTask.total > 0
                      ? {
                          width: `${Math.round(
                            (analysisTask.current / analysisTask.total) * 100,
                          )}%`,
                        }
                      : undefined
                  }
                />
              </div>
            </div>
          )}
        </div>
      </SettingsGroup>
      <SettingsGroup id="artwork" reveal={targetGroup === "artwork"}>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4 flex-1 min-w-0">
            <ImageIcon
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div className="min-w-0">
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.localArtistImages.title")}
              </div>
              <div className="text-xs settings-description">
                {localArtistRescanStatus ??
                  t("settings.localArtistImages.subtitle")}
              </div>
            </div>
          </div>
          <button
            type="button"
            onClick={handleRescanLocalArtistImages}
            disabled={isRescanningLocalArtists}
            className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <ImageIcon
              size={14}
              aria-hidden="true"
              className={isRescanningLocalArtists ? "animate-pulse" : ""}
            />
            <span>{t("settings.localArtistImages.action")}</span>
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
                {t("settings.artistImages.title")}
              </div>
              {artistFetchProgress && isFetchingArtists ? (
                <div className="text-xs text-zinc-500 mt-1 truncate">
                  {t("settings.artistImages.progress", {
                    current: artistFetchProgress.current,
                    total: artistFetchProgress.total,
                  })}
                  {artistFetchProgress.artistName
                    ? ` — ${artistFetchProgress.artistName}`
                    : ""}
                </div>
              ) : (
                <div className="text-xs settings-description">
                  {t("settings.artistImages.subtitle")}
                </div>
              )}
              {artistFetchProgress && artistFetchProgress.total > 0 && (
                <div className="mt-2 h-1.5 w-full max-w-xs rounded-full bg-zinc-200 dark:bg-zinc-700 overflow-hidden">
                  <div
                    className="h-full bg-emerald-500 transition-all"
                    style={{
                      width: `${Math.min(100, (artistFetchProgress.current / artistFetchProgress.total) * 100)}%`,
                    }}
                  />
                </div>
              )}
            </div>
          </div>
          <button
            type="button"
            onClick={handleFetchMissingArtistPictures}
            disabled={isFetchingArtists}
            className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <ImageIcon
              size={14}
              aria-hidden="true"
              className={isFetchingArtists ? "animate-pulse" : ""}
            />
            <span>{t("settings.artistImages.action")}</span>
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
                {t("library.fetchMissingCovers")}
              </div>
              {coverProgress && isFetchingCovers ? (
                <div className="text-xs text-zinc-500 mt-1 truncate">
                  {t("library.fetchingCovers", {
                    current: coverProgress.current,
                    total: coverProgress.total,
                  })}
                  {coverProgress.albumTitle
                    ? ` — ${coverProgress.albumTitle}`
                    : ""}
                </div>
              ) : coverResultMsg ? (
                <div className="text-xs text-emerald-600 dark:text-emerald-400 mt-1 truncate">
                  {coverResultMsg}
                </div>
              ) : (
                <div className="text-xs settings-description">
                  {t("library.fetchMissingCoversSubtitle")}
                </div>
              )}
              {coverProgress && coverProgress.total > 0 && (
                <div className="mt-2 h-1.5 w-full max-w-xs rounded-full bg-zinc-200 dark:bg-zinc-700 overflow-hidden">
                  <div
                    className="h-full bg-emerald-500 transition-all"
                    style={{
                      width: `${Math.min(100, (coverProgress.current / coverProgress.total) * 100)}%`,
                    }}
                  />
                </div>
              )}
            </div>
          </div>
          <button
            type="button"
            onClick={handleFetchMissingCovers}
            disabled={isFetchingCovers}
            className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <ImageIcon
              size={14}
              aria-hidden="true"
              className={isFetchingCovers ? "animate-pulse" : ""}
            />
            <span>{t("settings.artistImages.action")}</span>
          </button>
        </div>
      </SettingsGroup>
      <SettingsGroup id="biographies" reveal={targetGroup === "biographies"}>
        <ArtistBioSourceCard />
      </SettingsGroup>
      <SettingsGroup id="radio" reveal={targetGroup === "radio"}>
        <RadioCatalogueCard language={i18n.resolvedLanguage ?? i18n.language} />
      </SettingsGroup>
    </section>
  );
}

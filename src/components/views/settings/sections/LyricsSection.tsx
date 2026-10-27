import { useTranslation } from "react-i18next";
import { DesktopLyricsCard } from "../DesktopLyricsCard";
import { EstimatedKaraokeCard } from "../EstimatedKaraokeCard";
import { FullscreenLyricsCenteringCard } from "../FullscreenLyricsCenteringCard";
import { HeldNoteGlowCard } from "../HeldNoteGlowCard";
import { LyricsDestinationCard } from "../LyricsDestinationCard";
import { LyricsExcludedGenresCard } from "../LyricsExcludedGenresCard";
import { LyricsHighlightColorCard } from "../LyricsHighlightColorCard";
import { LyricsProvidersCard } from "../LyricsProvidersCard";
import { SettingsCategoryHeader } from "../SettingsCategoryHeader";
import { SettingsGroup } from "../SettingsGroup";
import { ToggleSwitch } from "../ToggleSwitch";
import { TRANSLATION_LANG_OPTIONS } from "../translationLanguages";
import { Globe, Mic2 } from "lucide-react";

interface LyricsSectionProps {
  handleCancelPrefetchLyrics: () => Promise<void>;
  handlePreferLrclibToggle: () => Promise<void>;
  handlePrefetchLyrics: () => Promise<void>;
  handleTranslationLangChange: (next: string) => Promise<void>;
  isPrefetchingLyrics: boolean;
  lyricsPrefetchProgress: {
    processed: number;
    total: number;
    hits: number;
    misses: number;
    failed: number;
    currentTitle: string | null;
  } | null;
  lyricsResultMsg: string | null;
  preferLrclib: boolean;
  preferLrclibBusy: boolean;
  targetGroup: string | null;
  translationLang: string | null;
  translationLangBusy: boolean;
}

export function LyricsSection({
  handleCancelPrefetchLyrics,
  handlePreferLrclibToggle,
  handlePrefetchLyrics,
  handleTranslationLangChange,
  isPrefetchingLyrics,
  lyricsPrefetchProgress,
  lyricsResultMsg,
  preferLrclib,
  preferLrclibBusy,
  targetGroup,
  translationLang,
  translationLangBusy,
}: LyricsSectionProps) {
  const { t } = useTranslation();
  return (
    <section
      aria-labelledby="settings-heading-media"
      className="settings-category"
    >
      <SettingsCategoryHeader category="media" />
      <SettingsGroup id="lyrics" reveal={targetGroup === "lyrics"}>
        <LyricsDestinationCard />
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4 flex-1 min-w-0">
            <Mic2 size={16} className="settings-row-icon" aria-hidden="true" />
            <div className="min-w-0">
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.lyricsPrefetch.title")}
              </div>
              {lyricsPrefetchProgress && isPrefetchingLyrics ? (
                <div className="text-xs text-zinc-500 mt-1 truncate">
                  {t("settings.lyricsPrefetch.progress", {
                    current: lyricsPrefetchProgress.processed,
                    total: lyricsPrefetchProgress.total,
                    hits: lyricsPrefetchProgress.hits,
                  })}
                  {lyricsPrefetchProgress.currentTitle
                    ? ` — ${lyricsPrefetchProgress.currentTitle}`
                    : ""}
                </div>
              ) : lyricsResultMsg ? (
                <div className="text-xs text-emerald-600 dark:text-emerald-400 mt-1 truncate">
                  {lyricsResultMsg}
                </div>
              ) : (
                <div className="text-xs settings-description">
                  {t("settings.lyricsPrefetch.subtitle")}
                </div>
              )}
              {lyricsPrefetchProgress && lyricsPrefetchProgress.total > 0 && (
                <div className="mt-2 h-1.5 w-full max-w-xs rounded-full bg-zinc-200 dark:bg-zinc-700 overflow-hidden">
                  <div
                    className="h-full bg-emerald-500 transition-all"
                    style={{
                      width: `${Math.min(100, (lyricsPrefetchProgress.processed / lyricsPrefetchProgress.total) * 100)}%`,
                    }}
                  />
                </div>
              )}
            </div>
          </div>
          {isPrefetchingLyrics ? (
            <button
              type="button"
              onClick={handleCancelPrefetchLyrics}
              className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
            >
              <span>{t("common.cancel")}</span>
            </button>
          ) : (
            <button
              type="button"
              onClick={handlePrefetchLyrics}
              className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <Mic2 size={14} aria-hidden="true" />
              <span>{t("settings.lyricsPrefetch.action")}</span>
            </button>
          )}
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4 flex-1 min-w-0">
            <Mic2 size={16} className="settings-row-icon" aria-hidden="true" />
            <div className="min-w-0">
              <label
                htmlFor="lyrics-translation-lang-select"
                className="text-sm font-medium text-zinc-900 dark:text-white"
              >
                {t("settings.lyricsTranslation.title")}
              </label>
              <div className="text-xs settings-description">
                {t("settings.lyricsTranslation.subtitle")}
              </div>
            </div>
          </div>
          <select
            id="lyrics-translation-lang-select"
            value={translationLang ?? ""}
            disabled={translationLangBusy}
            onChange={(e) => void handleTranslationLangChange(e.target.value)}
            className="shrink-0 px-3 py-2 rounded-xl border border-zinc-200 bg-white text-sm text-zinc-700 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {TRANSLATION_LANG_OPTIONS.map((opt) =>
              opt.value === "" ? (
                <option key="off" value="">
                  {t("settings.lyricsTranslation.off")}
                </option>
              ) : (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ),
            )}
          </select>
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4 flex-1 min-w-0">
            <Globe size={16} className="settings-row-icon" aria-hidden="true" />
            <div className="min-w-0">
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.lyricsPreferLrclib.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.lyricsPreferLrclib.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={preferLrclib}
            onToggle={() => void handlePreferLrclibToggle()}
            disabled={preferLrclibBusy}
            label={t("settings.lyricsPreferLrclib.title")}
          />
        </div>
        <LyricsProvidersCard />
        <LyricsExcludedGenresCard />
      </SettingsGroup>
      <SettingsGroup
        id="lyricsDisplay"
        reveal={targetGroup === "lyricsDisplay"}
      >
        <FullscreenLyricsCenteringCard />
        <EstimatedKaraokeCard />
        <HeldNoteGlowCard />
        <LyricsHighlightColorCard />
      </SettingsGroup>
      <SettingsGroup
        id="desktopLyrics"
        reveal={targetGroup === "desktopLyrics"}
      >
        <DesktopLyricsCard />
      </SettingsGroup>
    </section>
  );
}

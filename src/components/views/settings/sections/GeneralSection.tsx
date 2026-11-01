import { useTranslation } from "react-i18next";
import { normalizeSupportedLanguageCode } from "../../../../i18n";
import { LanguageDropdown } from "../LanguageDropdown";
import { SettingsCategoryHeader } from "../SettingsCategoryHeader";
import { SettingsGroup } from "../SettingsGroup";
import { ToggleSwitch } from "../ToggleSwitch";
import {
  openWhatsNew,
  replayOnboarding,
  startPlayerTour,
} from "../../../../lib/discovery";
import {
  useWhatsNewAutoOpen,
  useWhatsNewButton,
  useWhatsNewRelease,
} from "../../../../hooks/useWhatsNew";
import {
  Bell,
  Compass,
  Coffee,
  Disc3,
  Globe,
  ListPlus,
  Megaphone,
  Minimize2,
  MousePointerClick,
  Power,
} from "lucide-react";

interface GeneralSectionProps {
  autoStart: boolean;
  autoplay: boolean;
  autoplayResolved: boolean;
  generatorAlbumMode: boolean;
  generatorAlbumModeResolved: boolean;
  handleLanguageChange: (code: string) => void;
  handleToggleAutoStart: () => void;
  handleToggleKeepAwake: () => void;
  handleToggleMinimizeToTray: () => void;
  handleToggleSingleClickPlay: () => void;
  handleToggleTrackChangeNotif: () => void;
  keepAwake: boolean;
  minimizeToTray: boolean;
  setAutoplay: (next: boolean) => Promise<void>;
  setGeneratorAlbumMode: (next: boolean) => Promise<void>;
  singleClickPlay: boolean;
  targetGroup: string | null;
  trackChangeNotif: boolean;
}

/** Opens the panel, and holds the two choices about it. Stays here when the
 *  top bar's button is hidden: this is then the way back to it, with About. */
function WhatsNewRows() {
  const { t } = useTranslation();
  const release = useWhatsNewRelease();
  const autoOpen = useWhatsNewAutoOpen();
  const button = useWhatsNewButton();
  if (!release) return null;
  return (
    <>
      <div className="settings-row flex items-center justify-between">
        <div className="flex items-center space-x-4">
          <Megaphone
            size={16}
            className="settings-row-icon"
            aria-hidden="true"
          />
          <div>
            <div className="text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.whatsNew.title")}
            </div>
            <div className="text-xs settings-description">
              {t("settings.whatsNew.subtitle")}
            </div>
          </div>
        </div>
        <button
          type="button"
          onClick={openWhatsNew}
          className="shrink-0 rounded-full border border-zinc-200 px-3 py-1.5 text-xs font-semibold text-zinc-700 transition-colors hover:border-emerald-500 hover:text-emerald-600 dark:border-zinc-700 dark:text-zinc-200"
        >
          {t("settings.whatsNew.action")}
        </button>
      </div>
      <div className="settings-row flex items-center justify-between">
        <div className="min-w-0 ps-8">
          <div className="text-sm font-medium text-zinc-900 dark:text-white">
            {t("settings.whatsNew.autoOpen.title")}
          </div>
          <div className="text-xs settings-description">
            {t("settings.whatsNew.autoOpen.subtitle")}
          </div>
        </div>
        <ToggleSwitch
          enabled={autoOpen.value}
          onToggle={() => void autoOpen.setValue(!autoOpen.value)}
          disabled={!autoOpen.ready}
          label={t("settings.whatsNew.autoOpen.title")}
        />
      </div>
      <div className="settings-row flex items-center justify-between">
        <div className="min-w-0 ps-8">
          <div className="text-sm font-medium text-zinc-900 dark:text-white">
            {t("settings.whatsNew.button.title")}
          </div>
          <div className="text-xs settings-description">
            {t("settings.whatsNew.button.subtitle")}
          </div>
        </div>
        <ToggleSwitch
          enabled={button.value}
          onToggle={() => void button.setValue(!button.value)}
          disabled={!button.ready}
          label={t("settings.whatsNew.button.title")}
        />
      </div>
    </>
  );
}

export function GeneralSection({
  autoStart,
  autoplay,
  autoplayResolved,
  generatorAlbumMode,
  generatorAlbumModeResolved,
  handleLanguageChange,
  handleToggleAutoStart,
  handleToggleKeepAwake,
  handleToggleMinimizeToTray,
  handleToggleSingleClickPlay,
  handleToggleTrackChangeNotif,
  keepAwake,
  minimizeToTray,
  setAutoplay,
  setGeneratorAlbumMode,
  singleClickPlay,
  targetGroup,
  trackChangeNotif,
}: GeneralSectionProps) {
  const { i18n, t } = useTranslation();
  return (
    <section
      aria-labelledby="settings-heading-general"
      className="settings-category"
    >
      <SettingsCategoryHeader category="general" />
      <SettingsGroup id="language" reveal={targetGroup === "language"}>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Globe size={16} className="settings-row-icon" aria-hidden="true" />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.language.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.language.subtitle")}
              </div>
            </div>
          </div>
          <LanguageDropdown
            currentCode={normalizeSupportedLanguageCode(
              i18n.resolvedLanguage ?? i18n.language,
            )}
            onSelect={handleLanguageChange}
          />
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Compass
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.onboardingReplay.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.onboardingReplay.subtitle")}
              </div>
            </div>
          </div>
          <button
            type="button"
            onClick={replayOnboarding}
            className="shrink-0 rounded-full border border-zinc-200 px-3 py-1.5 text-xs font-semibold text-zinc-700 transition-colors hover:border-emerald-500 hover:text-emerald-600 dark:border-zinc-700 dark:text-zinc-200"
          >
            {t("settings.onboardingReplay.action")}
          </button>
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <MousePointerClick
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.playerTourReplay.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.playerTourReplay.subtitle")}
              </div>
            </div>
          </div>
          <button
            type="button"
            onClick={startPlayerTour}
            className="shrink-0 rounded-full border border-zinc-200 px-3 py-1.5 text-xs font-semibold text-zinc-700 transition-colors hover:border-emerald-500 hover:text-emerald-600 dark:border-zinc-700 dark:text-zinc-200"
          >
            {t("settings.playerTourReplay.action")}
          </button>
        </div>
        <WhatsNewRows />
      </SettingsGroup>
      <SettingsGroup id="startup" reveal={targetGroup === "startup"}>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Power size={16} className="settings-row-icon" aria-hidden="true" />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.autoStart.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.autoStart.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={autoStart}
            onToggle={handleToggleAutoStart}
            label={t("settings.autoStart.title")}
          />
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Minimize2
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.minimizeToTray.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.minimizeToTray.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={minimizeToTray}
            onToggle={handleToggleMinimizeToTray}
            label={t("settings.minimizeToTray.title")}
          />
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Bell size={16} className="settings-row-icon" aria-hidden="true" />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.integrations.notifications.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.integrations.notifications.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={trackChangeNotif}
            onToggle={handleToggleTrackChangeNotif}
            label={t("settings.integrations.notifications.title")}
          />
        </div>
      </SettingsGroup>
      <SettingsGroup id="listening" reveal={targetGroup === "listening"}>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <MousePointerClick
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.singleClickPlay.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.singleClickPlay.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={singleClickPlay}
            onToggle={handleToggleSingleClickPlay}
            label={t("settings.singleClickPlay.title")}
          />
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4 flex-1 min-w-0">
            <ListPlus
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div className="min-w-0">
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.autoplay.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.autoplay.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={autoplay}
            onToggle={() => void setAutoplay(!autoplay)}
            disabled={!autoplayResolved}
            label={t("settings.autoplay.title")}
          />
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4 flex-1 min-w-0">
            <Disc3 size={16} className="settings-row-icon" aria-hidden="true" />
            <div className="min-w-0">
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.generatorAlbumMode.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.generatorAlbumMode.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={generatorAlbumMode}
            onToggle={() => void setGeneratorAlbumMode(!generatorAlbumMode)}
            disabled={!generatorAlbumModeResolved}
            label={t("settings.generatorAlbumMode.title")}
          />
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Coffee
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.keepAwake.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.keepAwake.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={keepAwake}
            onToggle={handleToggleKeepAwake}
            label={t("settings.keepAwake.title")}
          />
        </div>
      </SettingsGroup>
    </section>
  );
}

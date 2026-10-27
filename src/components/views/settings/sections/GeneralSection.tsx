import { useTranslation } from "react-i18next";
import { normalizeSupportedLanguageCode } from "../../../../i18n";
import { LanguageDropdown } from "../LanguageDropdown";
import { SettingsCategoryHeader } from "../SettingsCategoryHeader";
import { SettingsGroup } from "../SettingsGroup";
import { ToggleSwitch } from "../ToggleSwitch";
import {
  Bell,
  Coffee,
  Disc3,
  Globe,
  ListPlus,
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

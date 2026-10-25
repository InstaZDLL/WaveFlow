import { useTranslation } from "react-i18next";
import { Palette } from "lucide-react";

import { useTheme } from "../../../hooks/useTheme";
import { THEME_PRESETS } from "../../../lib/themes";
import { SettingsChoiceCards } from "./SettingsChoiceCards";
import { ThemePreview } from "./settingsPreviews";

/**
 * Settings → Appearance → the theme: every preset as a card showing a
 * miniature window in its own colours — its ground, its sidebar, its
 * accent on a header, a play button and the progress.
 *
 * Adding a preset to `THEME_PRESETS` surfaces it here.
 */
export function ThemePickerCard() {
  const { t } = useTranslation();
  const { theme, setThemeId } = useTheme();

  return (
    <div className="settings-block">
      <div className="flex items-center space-x-4 mb-4">
        <Palette size={16} className="settings-row-icon" aria-hidden="true" />
        <div className="min-w-0">
          <div
            id="settings-theme-label"
            className="text-sm font-medium text-zinc-900 dark:text-white"
          >
            {t("settings.appearance.theme.title")}
          </div>
          <div className="text-xs settings-description">
            {t("settings.appearance.theme.subtitle")}
          </div>
        </div>
      </div>
      <SettingsChoiceCards
        labelledBy="settings-theme-label"
        minWidth={8.5}
        value={theme.id}
        onChange={(id, event) => void setThemeId(id, event)}
        choices={THEME_PRESETS.map((preset) => ({
          value: preset.id,
          label: t(preset.labelKey),
          hint: t(`settings.appearance.mode.${preset.mode}`),
          preview: <ThemePreview preset={preset} />,
        }))}
      />
    </div>
  );
}

import { useTranslation } from "react-i18next";
import { useSkin } from "../../../hooks/useSkin";
import { useTheme } from "../../../hooks/useTheme";
import { SKIN_PRESETS } from "../../../lib/skins";
import { THEME_PRESETS } from "../../../lib/themes";
import { SettingsChoiceCards } from "../../views/settings/SettingsChoiceCards";
import {
  SkinPreview,
  ThemePreview,
} from "../../views/settings/settingsPreviews";
import { StepHeading, StepSection } from "./StepHeading";
// The choice cards and their drawings are styled there; the Settings view
// is loaded lazily, so the journey may be the first to need them.
import "../../views/settings/settings.css";

/**
 * The look: the skin and the theme, with the cards and drawings of
 * Settings → Appearance, through the same setters. A choice applies at
 * once — the journey itself changes under the user's eyes.
 */
export function LookStep({ onTouch }: { onTouch: () => void }) {
  const { t } = useTranslation();
  const { skin, setSkinId } = useSkin();
  const { theme, setThemeId } = useTheme();

  return (
    <div>
      <StepHeading
        title={t("onboarding.look.title")}
        lede={t("onboarding.look.lede")}
      />
      <StepSection label={t("onboarding.look.skin")}>
        <SettingsChoiceCards
          label={t("onboarding.look.skin")}
          minWidth={10.5}
          value={skin.id}
          onChange={(id) => {
            onTouch();
            setSkinId(id);
          }}
          choices={SKIN_PRESETS.map((preset) => ({
            value: preset.id,
            label: (
              <span
                style={{
                  fontFamily: preset.typography.display,
                  fontWeight: preset.typography.headingWeight,
                  letterSpacing: preset.typography.displayTracking,
                }}
              >
                {t(preset.labelKey)}
              </span>
            ),
            hint: t(preset.descriptionKey),
            preview: <SkinPreview skinId={preset.id} />,
          }))}
        />
      </StepSection>
      <StepSection label={t("onboarding.look.theme")}>
        <SettingsChoiceCards
          label={t("onboarding.look.theme")}
          minWidth={7.5}
          value={theme.id}
          onChange={(id, event) => {
            onTouch();
            void setThemeId(id, event);
          }}
          choices={THEME_PRESETS.map((preset) => ({
            value: preset.id,
            label: t(preset.labelKey),
            hint: t(`settings.appearance.mode.${preset.mode}`),
            preview: <ThemePreview preset={preset} />,
          }))}
        />
      </StepSection>
    </div>
  );
}

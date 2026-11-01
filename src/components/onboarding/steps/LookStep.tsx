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
import { FeatureCard } from "../../discovery/FeatureCard";
import { useXwaylandChoice } from "../../../hooks/useXwaylandChoice";
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
      <WaylandWindows onTouch={onTouch} />
    </div>
  );
}

/**
 * On a Linux Wayland session only: the compositor ignores "always on
 * top" and places windows itself, so the mini-player and the desktop
 * lyrics cannot stay above or where they were put. The card says so and
 * offers XWayland — the choice of Settings → Appearance, through the same
 * hook (`useXwaylandChoice`), applied from the next start. Renders nothing
 * anywhere else.
 */
function WaylandWindows({ onTouch }: { onTouch: () => void }) {
  const { t } = useTranslation();
  const choice = useXwaylandChoice();
  const { status } = choice;
  if (!status) return null;
  const note = status.set_by_user
    ? t("settings.xwayland.setByUser")
    : choice.error
      ? choice.error
      : choice.pending
        ? t("settings.xwayland.nextStart")
        : status.active
          ? t("settings.xwayland.active")
          : null;
  return (
    <StepSection label={t("onboarding.look.windows")}>
      <FeatureCard
        art="miniPlayer"
        title={t("settings.xwayland.title")}
        benefit={t("settings.xwayland.subtitle")}
        checked={choice.on}
        disabled={choice.busy || status.set_by_user}
        onToggle={() => {
          onTouch();
          choice.toggle();
        }}
        status={
          note ? (
            <span
              className={
                choice.pending || choice.error
                  ? "text-amber-600 dark:text-amber-400"
                  : "text-zinc-500 dark:text-zinc-400"
              }
            >
              {note}
            </span>
          ) : undefined
        }
      />
    </StepSection>
  );
}

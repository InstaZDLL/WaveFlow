import { useTranslation } from "react-i18next";
import { Contrast } from "lucide-react";
import { useContrastMode } from "../../../hooks/useContrastMode";
import type { ContrastMode } from "../../../lib/contrast";
import { SettingsChoiceCards } from "./SettingsChoiceCards";
import { ContrastPreview } from "./settingsPreviews";

const MODES: ContrastMode[] = ["auto", "normal", "high"];

/**
 * Settings → Appearance row choosing the contrast level (#596).
 *
 * Three states rather than a switch, because the honest default is
 * neither on nor off: someone who has already told their operating
 * system they need more contrast should not have to find our toggle,
 * and someone who has deliberately turned the mode off should not have
 * it come back because the OS asked. `auto` is the default and defers
 * to `prefers-contrast: more`; the other two are explicit and win over
 * the OS in both directions.
 *
 * Each state is a card drawing the same text at its contrast, "auto"
 * showing both halves; the cards are a radio group, so the arrow keys
 * move between them.
 */
export function ContrastCard() {
  const { t } = useTranslation();
  const { mode, ready, setMode } = useContrastMode();

  return (
    <section
      aria-label={t("settings.contrast.title")}
      className="settings-block"
    >
      <div className="flex items-start gap-4">
        <Contrast size={16} className="settings-row-icon" aria-hidden="true" />
        <div className="min-w-0 flex-1">
          <span
            className="block text-sm font-medium text-zinc-900 dark:text-white"
            id="settings-contrast-label"
          >
            {t("settings.contrast.title")}
          </span>
          <span className="block text-xs mt-0.5 settings-description">
            {t("settings.contrast.subtitle")}
          </span>
        </div>
      </div>
      <div className="mt-4">
        <SettingsChoiceCards
          labelledBy="settings-contrast-label"
          minWidth={10}
          // Inert until the active profile's row has been read: the hook
          // answers with the default meanwhile, so a click before that
          // lands would persist a choice the user never made over one
          // they did.
          disabled={!ready}
          value={mode}
          onChange={(option) => void setMode(option)}
          choices={MODES.map((option) => ({
            value: option,
            label: t(`settings.contrast.modes.${option}.label`),
            hint: t(`settings.contrast.modes.${option}.description`),
            preview: <ContrastPreview mode={option} />,
          }))}
        />
      </div>
    </section>
  );
}

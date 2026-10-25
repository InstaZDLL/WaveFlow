import { useTranslation } from "react-i18next";
import { Sparkles } from "lucide-react";
import { useEffectsLevel } from "../../../hooks/useEffectsLevel";
import type { EffectsLevel } from "../../../lib/effects";
import { SettingsChoiceCards } from "./SettingsChoiceCards";
import { EffectsPreview } from "./settingsPreviews";

const LEVELS: EffectsLevel[] = ["auto", "full", "reduced"];

/**
 * Settings → Appearance row choosing how much the interface draws
 * besides its content (`lib/effects.ts`).
 *
 * Built like the contrast row beside it: three states, `auto` first and
 * the default, as cards in a radio group, each drawing a cover lighting
 * the page (full) or the same page flat (reduced). Under `auto` the card
 * says what it resolved to on this machine, since that depends on
 * something the user cannot see (whether the interface is drawn with the
 * GPU).
 */
export function EffectsCard() {
  const { t } = useTranslation();
  const { level, auto, ready, setLevel } = useEffectsLevel();

  return (
    <section
      aria-label={t("settings.effects.title")}
      className="settings-block"
    >
      <div className="flex items-start gap-4">
        <Sparkles size={16} className="settings-row-icon" aria-hidden="true" />
        <div className="min-w-0 flex-1">
          <span
            className="block text-sm font-medium text-zinc-900 dark:text-white"
            id="settings-effects-label"
          >
            {t("settings.effects.title")}
          </span>
          <span className="block text-xs mt-0.5 settings-description">
            {t("settings.effects.subtitle")}
          </span>
        </div>
      </div>
      <div className="mt-4">
        <SettingsChoiceCards
          labelledBy="settings-effects-label"
          minWidth={10}
          // Inert until the active profile's row has been read, as the
          // contrast row is.
          disabled={!ready}
          value={level}
          onChange={(option) => void setLevel(option)}
          choices={LEVELS.map((option) => ({
            value: option,
            label: t(`settings.effects.modes.${option}.label`),
            hint:
              option === "auto"
                ? t(`settings.effects.modes.auto.${auto}`)
                : t(`settings.effects.modes.${option}.description`),
            preview: <EffectsPreview level={option} />,
          }))}
        />
      </div>
    </section>
  );
}

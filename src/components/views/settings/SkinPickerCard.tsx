import { useTranslation } from "react-i18next";
import { Layers } from "lucide-react";
import { useSkin } from "../../../hooks/useSkin";
import { SKIN_PRESETS } from "../../../lib/skins";
import { SettingsChoiceCards } from "./SettingsChoiceCards";
import { SkinPreview } from "./settingsPreviews";

/**
 * Settings → Appearance → Skin picker.
 *
 * Where the theme picker swaps the accent palette, the skin picker
 * swaps the *language* of the UI: layout, surface materials,
 * typography, motion. The two axes are orthogonal so any (theme,
 * skin) combination works.
 *
 * Each card draws the skin's layout on its material — Studio's sidebar,
 * top bar and full-width bar; the fluid skins' rail, display title and
 * floating bar over the near-black, the aurora, the neon, the paper or
 * the wine room — and names it in the skin's own display face. Adding a
 * new skin to `SKIN_PRESETS` surfaces it here; its drawing is a
 * `.sp-skin--<id>` rule in `settings.css`.
 */
export function SkinPickerCard() {
  const { t } = useTranslation();
  const { skin, setSkinId, skinError } = useSkin();

  return (
    <section aria-labelledby="skin-picker-heading" className="settings-block">
      <div className="flex items-center space-x-4 mb-4">
        <Layers size={16} className="settings-row-icon" aria-hidden="true" />
        <div className="min-w-0">
          <div
            id="skin-picker-heading"
            className="text-sm font-medium text-zinc-900 dark:text-white"
          >
            {t("settings.appearance.skin.title")}
          </div>
          <div className="text-xs settings-description">
            {t("settings.appearance.skin.subtitle")}
          </div>
        </div>
      </div>
      {skinError && (
        <p role="alert" className="mb-3 text-xs text-red-600 dark:text-red-400">
          {t("skinRecovery.saveFailed")}
        </p>
      )}
      <SettingsChoiceCards
        labelledBy="skin-picker-heading"
        minWidth={12}
        value={skin.id}
        onChange={(id) => setSkinId(id)}
        choices={SKIN_PRESETS.map((preset) => ({
          value: preset.id,
          // The name in the skin's own display face: the cue that sells
          // the skin's mood before the user commits.
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
    </section>
  );
}

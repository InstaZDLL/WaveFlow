import { useTranslation } from "react-i18next";
import { Layers, Check } from "lucide-react";
import { useSkin } from "../../../hooks/useSkin";
import { SKIN_PRESETS } from "../../../lib/skins";

/**
 * Settings → Appearance → Skin picker.
 *
 * Where the theme picker swaps the accent palette, the skin picker
 * swaps the *language* of the UI: density, surface materials,
 * typography, motion. The two axes are orthogonal so any (theme,
 * skin) combination works.
 *
 * Each card shows a tiny preview of the skin's typographic identity
 * (display font + heading weight + tracking) over a swatch that
 * hints at the surface treatment (paper grain for Editorial, soft
 * shadow card for Studio). Adding a new skin to `SKIN_PRESETS`
 * surfaces it here automatically.
 */
export function SkinPickerCard() {
  const { t } = useTranslation();
  const { skin, setSkinId } = useSkin();

  return (
    <section
      aria-labelledby="skin-picker-heading"
      className="px-4 py-3"
    >
      <div className="flex items-center space-x-4 mb-4">
        <Layers
          size={20}
          className="text-zinc-400 shrink-0"
          aria-hidden="true"
        />
        <div className="min-w-0">
          <div
            id="skin-picker-heading"
            className="text-sm font-medium text-zinc-900 dark:text-white"
          >
            {t("settings.appearance.skin.title")}
          </div>
          <div className="text-xs text-zinc-400">
            {t("settings.appearance.skin.subtitle")}
          </div>
        </div>
      </div>
      <div
        className="grid grid-cols-2 gap-3"
        role="radiogroup"
        aria-labelledby="skin-picker-heading"
      >
        {SKIN_PRESETS.map((preset) => {
          const isActive = preset.id === skin.id;
          // Inline-style preview so the swatch always reflects the
          // skin's actual tokens, not the active skin's. Using inline
          // styles also sidesteps having to round-trip through
          // tailwind / data-skin attribute fiddling for the preview.
          const previewStyle: React.CSSProperties = {
            fontFamily: preset.typography.display,
            fontWeight: preset.typography.headingWeight,
            letterSpacing: preset.typography.displayTracking,
            borderRadius: preset.radius.card,
            boxShadow: preset.surface.shadowCard,
          };
          return (
            <button
              key={preset.id}
              type="button"
              role="radio"
              aria-checked={isActive}
              onClick={() => setSkinId(preset.id)}
              className={`group relative text-left overflow-hidden transition-all focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${
                isActive
                  ? "ring-2 ring-emerald-500/40"
                  : "ring-1 ring-zinc-200 dark:ring-zinc-700 hover:ring-zinc-300 dark:hover:ring-zinc-600"
              }`}
              style={{ borderRadius: preset.radius.card }}
            >
              <div
                className="px-4 py-5 bg-white dark:bg-zinc-900 relative"
                style={previewStyle}
              >
                {/* Skin label rendered in its own display family —
                    this is the headline visual cue that sells the
                    skin's mood before the user commits. */}
                <div className="text-base text-zinc-900 dark:text-white truncate">
                  {t(preset.labelKey)}
                </div>
                <div className="text-xs font-normal text-zinc-500 dark:text-zinc-400 mt-1 leading-relaxed font-sans">
                  {t(preset.descriptionKey)}
                </div>
                {isActive && (
                  <span
                    className="absolute top-2 right-2 inline-flex items-center justify-center w-5 h-5 rounded-full bg-emerald-500 text-white"
                    aria-hidden="true"
                  >
                    <Check size={12} strokeWidth={3} />
                  </span>
                )}
              </div>
            </button>
          );
        })}
      </div>
    </section>
  );
}

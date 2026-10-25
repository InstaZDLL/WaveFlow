import { useTranslation } from "react-i18next";
import { Sparkles } from "lucide-react";
import { useEffectsLevel } from "../../../hooks/useEffectsLevel";
import type { EffectsLevel } from "../../../lib/effects";

const LEVELS: EffectsLevel[] = ["auto", "full", "reduced"];

/**
 * Settings → Appearance row choosing how much the interface draws
 * besides its content (`lib/effects.ts`).
 *
 * Built like the contrast row beside it: three states, `auto` first and
 * the default, a radio group. Under `auto` the row says what it resolved
 * to on this machine, since that depends on something the user cannot
 * see (whether the interface is drawn with the GPU).
 */
export function EffectsCard() {
  const { t } = useTranslation();
  const { level, auto, ready, setLevel } = useEffectsLevel();

  return (
    <section aria-label={t("settings.effects.title")} className="px-4 py-3">
      <div className="flex items-start gap-3">
        <Sparkles
          size={20}
          className="text-zinc-400 mt-0.5 shrink-0"
          aria-hidden="true"
        />
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
          <div
            role="radiogroup"
            aria-labelledby="settings-effects-label"
            aria-busy={!ready}
            className={`mt-3 flex flex-col gap-2 ${ready ? "" : "opacity-50"}`}
          >
            {LEVELS.map((option) => (
              <label
                key={option}
                className="flex items-start gap-2.5 cursor-pointer"
              >
                <input
                  type="radio"
                  name="wf-effects"
                  value={option}
                  checked={level === option}
                  // Inert until the active profile's row has been read,
                  // as the contrast row is.
                  disabled={!ready}
                  onChange={() => {
                    void setLevel(option);
                  }}
                  className="mt-0.5 w-4 h-4 accent-emerald-500 cursor-pointer shrink-0 disabled:cursor-default"
                />
                <span className="min-w-0">
                  <span className="block text-sm text-zinc-800 dark:text-zinc-200">
                    {t(`settings.effects.modes.${option}.label`)}
                  </span>
                  <span className="block text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed">
                    {option === "auto"
                      ? t(`settings.effects.modes.auto.${auto}`)
                      : t(`settings.effects.modes.${option}.description`)}
                  </span>
                </span>
              </label>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}

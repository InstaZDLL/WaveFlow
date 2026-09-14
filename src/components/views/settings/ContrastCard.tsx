import { useTranslation } from "react-i18next";
import { Contrast } from "lucide-react";
import { useContrastMode } from "../../../hooks/useContrastMode";
import type { ContrastMode } from "../../../lib/contrast";

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
 * A radio group, not a segmented control built from buttons — the
 * options are mutually exclusive and arrow-key navigation between them
 * comes for free from the platform, which is worth more here than
 * anywhere else in Settings.
 */
export function ContrastCard() {
  const { t } = useTranslation();
  const { mode, setMode } = useContrastMode();

  return (
    <section aria-label={t("settings.contrast.title")} className="px-4 py-3">
      <div className="flex items-start gap-3">
        <Contrast
          size={20}
          className="text-zinc-400 mt-0.5 shrink-0"
          aria-hidden="true"
        />
        <div className="min-w-0 flex-1">
          <span
            className="block text-sm font-medium text-zinc-900 dark:text-white"
            id="settings-contrast-label"
          >
            {t("settings.contrast.title")}
          </span>
          <span className="block text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed mt-0.5">
            {t("settings.contrast.subtitle")}
          </span>
          <div
            role="radiogroup"
            aria-labelledby="settings-contrast-label"
            className="mt-3 flex flex-col gap-2"
          >
            {MODES.map((option) => (
              <label
                key={option}
                className="flex items-start gap-2.5 cursor-pointer"
              >
                <input
                  type="radio"
                  name="wf-contrast"
                  value={option}
                  checked={mode === option}
                  onChange={() => {
                    void setMode(option);
                  }}
                  className="mt-0.5 w-4 h-4 accent-emerald-500 cursor-pointer shrink-0"
                />
                <span className="min-w-0">
                  <span className="block text-sm text-zinc-800 dark:text-zinc-200">
                    {t(`settings.contrast.modes.${option}.label`)}
                  </span>
                  <span className="block text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed">
                    {t(`settings.contrast.modes.${option}.description`)}
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

import { useTranslation } from "react-i18next";
import { Sparkles } from "lucide-react";
import {
  useWrappedBannerVisibility,
  type WrappedBannerMode,
} from "../../../hooks/useWrappedBannerVisibility";

/**
 * Settings → Appearance card controlling the WaveFlow Wrapped banner
 * on the Home view. Three modes:
 *   - `auto` (default): only during Dec 1 → Jan 31 (Wrapped season)
 *   - `always`: visible year-round
 *   - `never`: never on Home (the view itself stays reachable)
 *
 * The Home banner also exposes a per-recap-year dismiss button. Picking
 * a mode here lifts that dismissal, and while one stands the card says
 * so with a button that lifts it: the radio already on `always` cannot
 * be picked again, and the banner would otherwise stay hidden all year.
 */
export function WrappedBannerCard() {
  const { t } = useTranslation();
  const { mode, setMode, inSeason, dismissedYear, undismiss } =
    useWrappedBannerVisibility();

  const options: Array<{
    value: WrappedBannerMode;
    labelKey: string;
    descriptionKey: string;
  }> = [
    {
      value: "auto",
      labelKey: "settings.wrappedBanner.modes.auto.label",
      descriptionKey: "settings.wrappedBanner.modes.auto.description",
    },
    {
      value: "always",
      labelKey: "settings.wrappedBanner.modes.always.label",
      descriptionKey: "settings.wrappedBanner.modes.always.description",
    },
    {
      value: "never",
      labelKey: "settings.wrappedBanner.modes.never.label",
      descriptionKey: "settings.wrappedBanner.modes.never.description",
    },
  ];

  return (
    <section
      aria-label={t("settings.wrappedBanner.title")}
      className="space-y-3"
    >
      <header className="flex items-start gap-4">
        <Sparkles size={16} className="settings-row-icon" aria-hidden="true" />
        <div className="min-w-0">
          <h3 className="text-sm font-semibold text-zinc-900 dark:text-white">
            {t("settings.wrappedBanner.title")}
          </h3>
          <p className="mt-1 text-xs settings-description">
            {t("settings.wrappedBanner.subtitle")}
          </p>
          {mode === "auto" && (
            <p
              className={`mt-2 text-xs font-medium ${
                inSeason
                  ? "text-emerald-600 dark:text-emerald-400"
                  : "text-zinc-400 dark:text-zinc-500"
              }`}
            >
              {inSeason
                ? t("settings.wrappedBanner.seasonActive")
                : t("settings.wrappedBanner.seasonIdle")}
            </p>
          )}
          {dismissedYear != null && mode !== "never" && (
            <p className="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-zinc-500 dark:text-zinc-400">
              <span>
                {t("settings.wrappedBanner.dismissed", { year: dismissedYear })}
              </span>
              <button
                type="button"
                onClick={() => {
                  void undismiss();
                }}
                className="rounded font-medium text-emerald-600 hover:underline focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 dark:text-emerald-400"
              >
                {t("settings.wrappedBanner.showAgain")}
              </button>
            </p>
          )}
        </div>
      </header>

      <fieldset>
        <legend className="sr-only">
          {t("settings.wrappedBanner.legend")}
        </legend>
        <div className="space-y-1">
          {options.map(({ value, labelKey, descriptionKey }) => {
            const checked = mode === value;
            return (
              <label
                key={value}
                className="flex items-start gap-3 px-3 py-2 rounded-lg cursor-pointer hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors"
              >
                <input
                  type="radio"
                  name="wrapped-banner-mode"
                  value={value}
                  checked={checked}
                  onChange={() => {
                    void setMode(value);
                  }}
                  className="mt-0.5 w-4 h-4 accent-emerald-500 cursor-pointer"
                />
                <span className="min-w-0">
                  <span className="block text-sm text-zinc-800 dark:text-zinc-200">
                    {t(labelKey)}
                  </span>
                  <span className="block text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed">
                    {t(descriptionKey)}
                  </span>
                </span>
              </label>
            );
          })}
        </div>
      </fieldset>
    </section>
  );
}

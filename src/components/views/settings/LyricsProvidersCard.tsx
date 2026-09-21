import { useTranslation } from "react-i18next";
import { Globe } from "lucide-react";
import {
  SWITCHABLE_LYRICS_PROVIDERS,
  useDisabledLyricsProviders,
} from "../../../hooks/useLyricsLookupSettings";

/**
 * Settings → Lyrics card switching the online providers of the lookup
 * on and off (#722). A switched-off provider is never asked on its own —
 * not when a panel opens, not in the bulk prefetch — though it can still
 * be picked by name in the lyrics panel. Lyrics it already supplied stay
 * cached; the subtitle says so, or the switch would look like it did
 * nothing.
 */
export function LyricsProvidersCard() {
  const { t } = useTranslation();
  const { disabled, ready, setEnabled } = useDisabledLyricsProviders();

  return (
    <section
      aria-label={t("settings.lyricsProviders.title")}
      className="space-y-3 py-3"
    >
      <header className="px-4 flex items-start gap-3">
        <Globe
          size={20}
          className="text-zinc-400 mt-0.5 shrink-0"
          aria-hidden="true"
        />
        <div className="min-w-0">
          <h3 className="text-sm font-medium text-zinc-900 dark:text-white">
            {t("settings.lyricsProviders.title")}
          </h3>
          <p className="mt-0.5 text-xs settings-description">
            {t("settings.lyricsProviders.subtitle")}
          </p>
        </div>
      </header>

      <fieldset className="mx-4">
        <legend className="sr-only">
          {t("settings.lyricsProviders.title")}
        </legend>
        <div className="space-y-1">
          <label className="flex items-center gap-3 px-3 py-2 rounded-lg cursor-not-allowed">
            <input
              type="checkbox"
              checked
              disabled
              className="w-4 h-4 accent-emerald-500 cursor-not-allowed"
            />
            <span className="text-sm text-zinc-800 dark:text-zinc-200">
              {t("lyrics.provider.lrclib")}
            </span>
            <span className="text-xs settings-description">
              {t("settings.lyricsProviders.alwaysOn")}
            </span>
          </label>
          {SWITCHABLE_LYRICS_PROVIDERS.map((provider) => (
            <label
              key={provider}
              className={`flex items-center gap-3 px-3 py-2 rounded-lg transition-colors ${
                ready
                  ? "cursor-pointer hover:bg-zinc-50 dark:hover:bg-zinc-800/30"
                  : "cursor-not-allowed opacity-50"
              }`}
            >
              <input
                type="checkbox"
                checked={!disabled.includes(provider)}
                // Until the profile's value lands, `disabled` is the
                // default, and a click would persist the wrong list.
                disabled={!ready}
                onChange={(e) => {
                  setEnabled(provider, e.target.checked);
                }}
                className="w-4 h-4 accent-emerald-500 cursor-pointer disabled:cursor-not-allowed"
              />
              <span className="text-sm text-zinc-800 dark:text-zinc-200">
                {t(`lyrics.provider.${provider}`)}
              </span>
              {provider === "genius" && (
                <span className="text-xs settings-description">
                  {t("settings.lyricsProviders.geniusNote")}
                </span>
              )}
            </label>
          ))}
        </div>
      </fieldset>
    </section>
  );
}

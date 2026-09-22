import { useTranslation } from "react-i18next";
import { Globe, Lock } from "lucide-react";
import {
  SWITCHABLE_LYRICS_PROVIDERS,
  useDisabledLyricsProviders,
} from "../../../hooks/useLyricsLookupSettings";
import { ToggleSwitch } from "../../common/ToggleSwitch";

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
          {/* LRCLIB cannot be turned off, so it gets no control at all.
              A switch that is permanently on invites the click it will
              refuse; the note beside the name says why instead. */}
          <div className="flex items-center justify-between gap-3 px-3 py-2 rounded-lg">
            <span className="flex items-center gap-3 min-w-0">
              <span className="text-sm text-zinc-800 dark:text-zinc-200">
                {t("lyrics.provider.lrclib")}
              </span>
              <span className="text-xs settings-description">
                {t("settings.lyricsProviders.alwaysOn")}
              </span>
            </span>
            <Lock
              size={16}
              className="shrink-0 text-zinc-400 dark:text-zinc-500"
              aria-hidden="true"
            />
          </div>
          {SWITCHABLE_LYRICS_PROVIDERS.map((provider) => (
            <div
              key={provider}
              className={`flex items-center justify-between gap-3 px-3 py-2 rounded-lg transition-colors ${
                ready
                  ? "hover:bg-zinc-50 dark:hover:bg-zinc-800/30"
                  : "opacity-50"
              }`}
            >
              <span className="flex items-center gap-3 min-w-0">
                <span className="text-sm text-zinc-800 dark:text-zinc-200">
                  {t(`lyrics.provider.${provider}`)}
                </span>
                {provider === "genius" && (
                  <span className="text-xs settings-description">
                    {t("settings.lyricsProviders.geniusNote")}
                  </span>
                )}
              </span>
              {/* The stored value is the list of the *disabled* ones, so
                  the flip is easy to get backwards: switching on means
                  removing from that list. */}
              <ToggleSwitch
                enabled={!disabled.includes(provider)}
                // Until the profile's value lands, `disabled` is the
                // default, and a click would persist the wrong list.
                disabled={!ready}
                onToggle={() => {
                  setEnabled(provider, disabled.includes(provider));
                }}
                label={t(`lyrics.provider.${provider}`)}
              />
            </div>
          ))}
        </div>
      </fieldset>
    </section>
  );
}

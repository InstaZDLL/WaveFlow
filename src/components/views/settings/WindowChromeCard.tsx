import { useTranslation } from "react-i18next";
import { AppWindow } from "lucide-react";

import { useWindowChrome } from "../../../hooks/useWindowChrome";
import { SettingsChoiceCards } from "./SettingsChoiceCards";
import { WindowFramePreview } from "./settingsPreviews";

const OPTIONS = ["system", "app"] as const;

/**
 * Settings → Appearance row picking who draws the window's frame
 * (issue #696): two cards, each drawing the window with the system's
 * title bar above it or with WaveFlow up to the top edge.
 *
 * **Renders nothing where the choice is not offered.** Today that means
 * Windows, where a frame we drew ourselves would lose Snap Layouts and the
 * system menu — a worse Windows than the one the user has. The platform
 * rule lives in `commands/preferences.rs`, not here; this card only asks
 * whether there is a choice to show.
 */
export function WindowChromeCard() {
  const { t } = useTranslation();
  const { chrome, supported, ready, busy, choose } = useWindowChrome();

  if (!ready || !supported) return null;

  return (
    <section
      aria-labelledby="settings-window-chrome-heading"
      className="settings-block"
    >
      <header className="flex items-start gap-4 mb-4">
        <AppWindow size={16} className="settings-row-icon" aria-hidden="true" />
        <div className="min-w-0">
          <h3
            id="settings-window-chrome-heading"
            className="text-sm font-medium text-zinc-900 dark:text-white"
          >
            {t("settings.windowChrome.title")}
          </h3>
          <p className="text-xs mt-0.5 settings-description">
            {t("settings.windowChrome.subtitle")}
          </p>
        </div>
      </header>

      <SettingsChoiceCards
        labelledBy="settings-window-chrome-heading"
        minWidth={11}
        disabled={busy}
        value={chrome}
        onChange={(id) => void choose(id)}
        choices={OPTIONS.map((id) => ({
          value: id,
          label: t(`settings.windowChrome.options.${id}.label`),
          hint: t(`settings.windowChrome.options.${id}.hint`),
          preview: <WindowFramePreview kind={id} />,
        }))}
      />
    </section>
  );
}

import { useTranslation } from "react-i18next";
import { AppWindow, PanelTop } from "lucide-react";

import { useWindowChrome } from "../../../hooks/useWindowChrome";

const OPTIONS: ReadonlyArray<{
  id: "system" | "app";
  Icon: typeof AppWindow;
}> = [
  { id: "system", Icon: AppWindow },
  { id: "app", Icon: PanelTop },
];

/**
 * Settings → Appearance row picking who draws the window's frame
 * (issue #696).
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
      className="px-4 py-3"
    >
      <header className="flex items-start gap-3 mb-3">
        <AppWindow
          size={20}
          className="text-zinc-400 mt-0.5 shrink-0"
          aria-hidden="true"
        />
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

      <div
        role="radiogroup"
        aria-labelledby="settings-window-chrome-heading"
        className="grid grid-cols-1 sm:grid-cols-2 gap-2"
      >
        {OPTIONS.map(({ id, Icon }) => {
          const selected = chrome === id;
          return (
            <button
              key={id}
              type="button"
              role="radio"
              aria-checked={selected}
              disabled={busy}
              onClick={() => void choose(id)}
              className={[
                "flex flex-col items-start gap-2 rounded-xl border p-3 text-left transition-all disabled:opacity-50",
                selected
                  ? "border-emerald-500 bg-emerald-50 dark:bg-emerald-950/30 ring-1 ring-emerald-500/40"
                  : "border-zinc-200 dark:border-zinc-700 hover:border-zinc-300 dark:hover:border-zinc-600 bg-white dark:bg-zinc-900",
              ].join(" ")}
            >
              <Icon
                size={18}
                className={
                  selected
                    ? "text-emerald-600 dark:text-emerald-400"
                    : "text-zinc-400"
                }
                aria-hidden="true"
              />
              <span className="text-sm font-medium text-zinc-900 dark:text-white">
                {t(`settings.windowChrome.options.${id}.label`)}
              </span>
              <span className="text-xs text-zinc-500 dark:text-zinc-400 leading-snug">
                {t(`settings.windowChrome.options.${id}.hint`)}
              </span>
            </button>
          );
        })}
      </div>
    </section>
  );
}

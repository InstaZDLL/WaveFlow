import { useTranslation } from "react-i18next";
import { Layers } from "lucide-react";

import { useXwaylandChoice } from "../../../hooks/useXwaylandChoice";
import { ToggleSwitch } from "../../common/ToggleSwitch";

/**
 * Run through XWayland on a Linux Wayland session (#894).
 *
 * Wayland gives an app no way to stay above other windows or to choose
 * where its window sits, so the mini-player's pin and the desktop lyrics'
 * "always on top" are ignored there. XWayland brings both back, at the
 * cost of less crisp text under fractional scaling, which is why it is a
 * choice and not the default.
 *
 * **Renders nothing off a Wayland session**: Windows, macOS and X11 have
 * nothing to choose. Applies from the next start; the card says whether
 * this launch already runs that way.
 */
export function XwaylandCard() {
  const { t } = useTranslation();
  const { status, busy, error, on, pending, toggle } = useXwaylandChoice();

  if (!status) return null;

  return (
    <div className="settings-block">
      <div className="flex items-center justify-between gap-4">
        <div className="flex items-center space-x-4 min-w-0">
          <Layers size={16} className="settings-row-icon" aria-hidden="true" />
          <div className="min-w-0">
            <p className="text-sm font-medium text-zinc-800 dark:text-zinc-200">
              {t("settings.xwayland.title")}
            </p>
            <p className="text-xs mt-0.5 settings-description">
              {t("settings.xwayland.subtitle")}
            </p>
          </div>
        </div>
        <ToggleSwitch
          enabled={on}
          onToggle={toggle}
          disabled={busy || status.set_by_user}
          label={t("settings.xwayland.title")}
        />
      </div>
      {status.set_by_user ? (
        <p className="text-xs settings-description mt-2 ml-9">
          {t("settings.xwayland.setByUser")}
        </p>
      ) : error ? (
        <p className="text-xs text-amber-600 dark:text-amber-400 mt-2 ml-9">
          {error}
        </p>
      ) : pending ? (
        <p className="text-xs text-amber-600 dark:text-amber-400 mt-2 ml-9">
          {t("settings.xwayland.nextStart")}
        </p>
      ) : status.active ? (
        <p className="text-xs settings-description mt-2 ml-9">
          {t("settings.xwayland.active")}
        </p>
      ) : null}
    </div>
  );
}

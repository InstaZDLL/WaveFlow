import { useTranslation } from "react-i18next";
import { FlaskConical } from "lucide-react";
import { useUpdateChannel } from "../../../hooks/useUpdateChannel";
import { UPDATER_RECHECK_EVENT } from "../../../lib/tauri/updater";
import { ToggleSwitch } from "../../common/ToggleSwitch";

/**
 * Settings → Diagnostics row opting the in-app updater into pre-release
 * builds. Default **off** (stable). When toggled, the choice persists to
 * `app_setting['updater.channel']` and a window event nudges the
 * UpdateBanner's `useUpdater` to re-check against the new endpoint
 * immediately (no relaunch). Betas are served from the rolling
 * `beta-channel` manifest; stable users never see them.
 */
export function UpdateChannelCard() {
  const { t } = useTranslation();
  const { channel, loaded, setChannel } = useUpdateChannel();

  const onToggle = async (enabled: boolean) => {
    try {
      await setChannel(enabled ? "beta" : "stable");
    } catch {
      // Write failed; the hook already rolled the state back. Skip the
      // re-check so the banner doesn't probe a channel that didn't stick.
      return;
    }
    if (typeof window !== "undefined") {
      window.dispatchEvent(new CustomEvent(UPDATER_RECHECK_EVENT));
    }
  };

  return (
    <section
      aria-label={t("settings.updateChannel.title")}
      className="px-4 py-3"
    >
      <div className="flex items-center justify-between gap-3">
        <span className="flex items-start gap-3 min-w-0">
          <FlaskConical
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.updateChannel.title")}
            </span>
            <span className="block text-xs mt-0.5 settings-description">
              {t("settings.updateChannel.subtitle")}
            </span>
          </span>
        </span>
        {/* A boolean face on a `stable` / `beta` string, so the flip is
            against the channel rather than a checkbox event. */}
        <ToggleSwitch
          enabled={channel === "beta"}
          onToggle={() => {
            void onToggle(channel !== "beta");
          }}
          label={t("settings.updateChannel.title")}
          disabled={!loaded}
        />
      </div>
    </section>
  );
}

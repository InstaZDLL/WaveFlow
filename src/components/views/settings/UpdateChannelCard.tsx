import { useTranslation } from "react-i18next";
import { FlaskConical } from "lucide-react";
import { useUpdateChannel } from "../../../hooks/useUpdateChannel";
import { UPDATER_RECHECK_EVENT } from "../../../lib/tauri/updater";

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
    await setChannel(enabled ? "beta" : "stable");
    if (typeof window !== "undefined") {
      window.dispatchEvent(new CustomEvent(UPDATER_RECHECK_EVENT));
    }
  };

  return (
    <section
      aria-label={t("settings.updateChannel.title")}
      className="px-4 py-3"
    >
      <label className="flex items-start justify-between gap-3 cursor-pointer">
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
            <span className="block text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed mt-0.5">
              {t("settings.updateChannel.subtitle")}
            </span>
          </span>
        </span>
        <input
          type="checkbox"
          checked={channel === "beta"}
          disabled={!loaded}
          onChange={(e) => {
            void onToggle(e.target.checked);
          }}
          className="mt-1.5 w-4 h-4 accent-emerald-500 cursor-pointer shrink-0 disabled:opacity-50"
          aria-label={t("settings.updateChannel.title")}
        />
      </label>
    </section>
  );
}

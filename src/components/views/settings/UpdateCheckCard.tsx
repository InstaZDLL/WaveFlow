import { useState } from "react";
import { useTranslation } from "react-i18next";
import { RefreshCw } from "lucide-react";
import { useUpdateAutoCheck } from "../../../hooks/useUpdateAutoCheck";
import {
  checkForUpdate,
  isUpdaterOfflineError,
  UPDATER_FOUND_EVENT,
} from "../../../lib/tauri/updater";
import { ToggleSwitch } from "../../common/ToggleSwitch";

type CheckStatus =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "upToDate" }
  | { kind: "available"; version: string }
  | { kind: "offline" }
  | { kind: "failed" };

/**
 * Settings → Maintenance → Updates: whether the updater looks for a new
 * version on its own, and a check made on demand.
 *
 * The switch is saved first and moves once the backend has the value
 * (`useUpdateAutoCheck`); a save that fails leaves it where it was and
 * says so. Off, the launch and a channel switch read no manifest, and
 * "Check now" is how the user looks. A check made here that finds a
 * build hands it to the UpdateBanner, which offers the install.
 */
export function UpdateCheckCard() {
  const { t } = useTranslation();
  const { autoCheck, ready, setAutoCheck } = useUpdateAutoCheck();
  const [saving, setSaving] = useState(false);
  const [saveFailed, setSaveFailed] = useState(false);
  const [status, setStatus] = useState<CheckStatus>({ kind: "idle" });

  const onToggle = () => {
    setSaving(true);
    setSaveFailed(false);
    setAutoCheck(!autoCheck)
      .catch((err) => {
        console.error("[UpdateCheckCard] save failed", err);
        setSaveFailed(true);
      })
      .finally(() => setSaving(false));
  };

  const onCheckNow = async () => {
    setStatus({ kind: "checking" });
    try {
      const update = await checkForUpdate({ automatic: false });
      if (update) {
        setStatus({ kind: "available", version: update.version });
        window.dispatchEvent(
          new CustomEvent(UPDATER_FOUND_EVENT, { detail: update }),
        );
      } else {
        setStatus({ kind: "upToDate" });
      }
    } catch (err) {
      if (isUpdaterOfflineError(err)) {
        setStatus({ kind: "offline" });
      } else {
        console.error("[UpdateCheckCard] check failed", err);
        setStatus({ kind: "failed" });
      }
    }
  };

  const statusText =
    status.kind === "checking"
      ? t("settings.updateCheck.checking")
      : status.kind === "upToDate"
        ? t("settings.updateCheck.upToDate")
        : status.kind === "available"
          ? t("settings.updateCheck.available", { version: status.version })
          : status.kind === "offline"
            ? t("settings.updateCheck.offline")
            : status.kind === "failed"
              ? t("settings.updateCheck.failed")
              : null;

  return (
    <section aria-label={t("settings.updateCheck.title")} className="px-4 py-3">
      <div className="flex items-center justify-between gap-3">
        <span className="flex items-start gap-4 min-w-0">
          <RefreshCw
            size={16}
            className="settings-row-icon"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.updateCheck.title")}
            </span>
            <span className="block text-xs mt-0.5 settings-description">
              {t("settings.updateCheck.subtitle")}
            </span>
          </span>
        </span>
        <div className="flex items-center gap-3 shrink-0">
          <button
            type="button"
            onClick={() => void onCheckNow()}
            disabled={status.kind === "checking"}
            className="px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 disabled:opacity-60 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
          >
            {t("settings.updateCheck.checkNow")}
          </button>
          <ToggleSwitch
            enabled={autoCheck}
            onToggle={onToggle}
            label={t("settings.updateCheck.title")}
            disabled={!ready || saving}
          />
        </div>
      </div>
      <div aria-live="polite">
        {statusText && (
          <div className="mt-2 text-xs settings-description">{statusText}</div>
        )}
      </div>
      {saveFailed && (
        <div role="alert" className="mt-2 text-xs text-rose-500 break-words">
          {t("settings.updateCheck.saveFailed")}
        </div>
      )}
    </section>
  );
}

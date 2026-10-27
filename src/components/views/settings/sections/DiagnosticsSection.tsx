import type { Dispatch, SetStateAction } from "react";
import { useTranslation } from "react-i18next";
import { RenderingCard } from "../RenderingCard";
import { SettingsCategoryHeader } from "../SettingsCategoryHeader";
import { SettingsGroup } from "../SettingsGroup";
import { UpdateChannelCard } from "../UpdateChannelCard";
import {
  Check as CheckIcon,
  Copy,
  FileText,
  FolderOpen,
  Trash2,
} from "lucide-react";

interface DiagnosticsSectionProps {
  copyLogsStatus: "ok" | "fail" | null;
  handleCopyLogs: () => Promise<void>;
  handleOpenLogFolder: () => void;
  setIsResetOpen: Dispatch<SetStateAction<boolean>>;
  targetGroup: string | null;
}

export function DiagnosticsSection({
  copyLogsStatus,
  handleCopyLogs,
  handleOpenLogFolder,
  setIsResetOpen,
  targetGroup,
}: DiagnosticsSectionProps) {
  const { t } = useTranslation();
  return (
    <section
      aria-labelledby="settings-heading-diagnostics"
      className="settings-category"
    >
      <SettingsCategoryHeader category="diagnostics" />
      <SettingsGroup id="updates" reveal={targetGroup === "updates"}>
        <UpdateChannelCard />
      </SettingsGroup>
      <SettingsGroup id="diagnostics" reveal={targetGroup === "diagnostics"}>
        <RenderingCard />
        <div className="settings-block">
          <div className="flex items-center justify-between gap-4">
            <div className="flex items-center space-x-4 min-w-0">
              <FileText
                size={16}
                className="settings-row-icon"
                aria-hidden="true"
              />
              <div className="min-w-0">
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.diagnostics.title")}
                </div>
                <div className="text-xs settings-description">
                  {t("settings.diagnostics.subtitle")}
                </div>
              </div>
            </div>
            <div className="flex items-center space-x-2 shrink-0">
              <button
                type="button"
                onClick={handleOpenLogFolder}
                className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
              >
                <FolderOpen size={14} aria-hidden="true" />
                <span>{t("settings.diagnostics.openFolder")}</span>
              </button>
              <button
                type="button"
                onClick={handleCopyLogs}
                className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
              >
                {copyLogsStatus === "ok" ? (
                  <CheckIcon
                    size={14}
                    aria-hidden="true"
                    className="text-emerald-500"
                  />
                ) : (
                  <Copy size={14} aria-hidden="true" />
                )}
                <span>
                  {copyLogsStatus === "ok"
                    ? t("settings.diagnostics.copied")
                    : copyLogsStatus === "fail"
                      ? t("settings.diagnostics.copyFailed")
                      : t("settings.diagnostics.copyLogs")}
                </span>
              </button>
            </div>
          </div>
        </div>
      </SettingsGroup>
      <SettingsGroup id="reset" advanced reveal={targetGroup === "reset"}>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Trash2
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.reset.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.reset.subtitle")}
              </div>
            </div>
          </div>
          <button
            type="button"
            onClick={() => setIsResetOpen(true)}
            aria-label={t("settings.reset.action")}
            className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-red-200 bg-white text-sm font-medium text-red-500 hover:bg-red-50 dark:border-red-500/30 dark:bg-zinc-800 dark:hover:bg-red-500/10 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-red-500"
          >
            <Trash2 size={14} aria-hidden="true" />
            <span>{t("settings.reset.action")}</span>
          </button>
        </div>
      </SettingsGroup>
    </section>
  );
}

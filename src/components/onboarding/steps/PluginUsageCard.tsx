import { useTranslation } from "react-i18next";
import { Check, Download, Loader2, Puzzle } from "lucide-react";
import { useLocalizedText } from "../../../hooks/useLocalizedText";
import type { MarketplaceEntry } from "../../../lib/tauri/plugins";
import { FeatureCard } from "../../discovery/FeatureCard";
import {
  USAGE_ART,
  WORD_SYNCED_TAG,
  type InstallState,
  type PluginUsage,
} from "../usePluginCatalogue";

/**
 * One usage of the store (lyrics, Canvas, animated covers…) as a card,
 * with the recommended plugins for it underneath, each installed in a
 * click. With no store to read (offline, or no registry answered) it only
 * presents the usage and says where to find it later.
 */
export function PluginUsageCard({
  usage,
  entries,
  available,
  installs,
  onInstall,
}: {
  usage: PluginUsage;
  entries: MarketplaceEntry[];
  /** `false` while the store cannot be read. */
  available: boolean;
  installs: Record<string, InstallState>;
  onInstall: (id: string) => void;
}) {
  const { t } = useTranslation();
  const localized = useLocalizedText();
  const wordSynced =
    usage === "lyrics" && entries.some((e) => e.tags.includes(WORD_SYNCED_TAG));

  return (
    <FeatureCard
      art={USAGE_ART[usage]}
      title={t(`onboarding.plugins.usage.${usage}.title`)}
      benefit={t(`onboarding.plugins.usage.${usage}.benefit`)}
      badges={wordSynced ? [t("onboarding.plugins.wordSynced")] : undefined}
      checked={entries.some((e) => e.installed)}
    >
      {!available || entries.length === 0 ? (
        <p className="mt-3 flex items-center gap-1.5 text-xs text-zinc-500 dark:text-zinc-400">
          <Puzzle size={13} aria-hidden="true" className="shrink-0" />
          {t("onboarding.plugins.inExtensions")}
        </p>
      ) : (
        <ul className="mt-3 space-y-1.5">
          {entries.map((entry) => {
            const state = installs[entry.id];
            const description = localized(entry.description);
            return (
              <li
                key={entry.id}
                className="flex items-center gap-3 rounded-xl bg-zinc-50 px-3 py-2 dark:bg-white/5"
              >
                <div className="min-w-0 flex-1">
                  <div className="truncate text-xs font-semibold text-zinc-900 dark:text-white">
                    {entry.name}
                  </div>
                  {state?.kind === "failed" ? (
                    <div
                      className="truncate text-[11px] text-rose-500"
                      role="alert"
                      title={state.message}
                    >
                      {t("onboarding.plugins.failed")}
                    </div>
                  ) : (
                    description && (
                      <div
                        className="truncate text-[11px] text-zinc-500 dark:text-zinc-400"
                        title={description}
                      >
                        {description}
                      </div>
                    )
                  )}
                </div>
                {entry.installed ? (
                  <span className="inline-flex shrink-0 items-center gap-1 text-[11px] font-semibold text-emerald-600 dark:text-emerald-400">
                    <Check size={13} aria-hidden="true" />
                    {t("onboarding.plugins.installed")}
                  </span>
                ) : (
                  <button
                    type="button"
                    onClick={() => onInstall(entry.id)}
                    disabled={state?.kind === "installing"}
                    aria-label={t("onboarding.plugins.installAria", {
                      name: entry.name,
                    })}
                    className="onboarding-ghost inline-flex shrink-0 items-center gap-1 rounded-full border border-zinc-200 px-2.5 py-1 text-[11px] font-semibold text-zinc-700 transition-colors hover:border-emerald-500 hover:text-emerald-600 disabled:cursor-wait disabled:opacity-60 dark:border-zinc-700 dark:text-zinc-200"
                  >
                    {state?.kind === "installing" ? (
                      <Loader2 size={12} className="animate-spin" />
                    ) : (
                      <Download size={12} aria-hidden="true" />
                    )}
                    {state?.kind === "installing"
                      ? t("onboarding.plugins.installing")
                      : t("onboarding.plugins.install")}
                  </button>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </FeatureCard>
  );
}

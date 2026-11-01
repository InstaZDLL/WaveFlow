import { useTranslation } from "react-i18next";
import { Loader2 } from "lucide-react";
import {
  PLUGIN_USAGES,
  usageOf,
  type PluginUsage,
  usePluginCatalogue,
} from "../usePluginCatalogue";
import { PluginUsageCard } from "./PluginUsageCard";
import { StepHeading } from "./StepHeading";

/** The usages told even without a store to read: what the registry is
 *  known to carry. Web Radio ships with the app and needs no install. */
const PRESENTED_OFFLINE: readonly PluginUsage[] = [
  "lyrics",
  "canvas",
  "artwork",
];

/**
 * Plugins: the store's first-party picks this build can run, grouped by
 * what they are for, installed straight from here. Offline, the usages
 * stay on screen as a presentation pointing at Extensions.
 */
export function PluginsStep({
  onTouch,
  plugins,
}: {
  onTouch: () => void;
  plugins: ReturnType<typeof usePluginCatalogue>;
}) {
  const { t } = useTranslation();
  const { catalogue, installs, install } = plugins;

  const entries = catalogue.status === "ready" ? catalogue.entries : [];
  const found = PLUGIN_USAGES.filter((usage) =>
    entries.some((e) => usageOf(e) === usage),
  );
  // A store with nothing to recommend reads as one that could not be
  // reached: the usages are presented, and Extensions is where to look.
  const available = found.length > 0;
  const usages: readonly PluginUsage[] = available ? found : PRESENTED_OFFLINE;

  return (
    <div>
      <StepHeading
        title={t("onboarding.plugins.title")}
        lede={t("onboarding.plugins.lede")}
      />
      {catalogue.status === "loading" ? (
        <div className="mt-10 flex items-center justify-center gap-2 text-sm text-zinc-500">
          <Loader2 size={16} className="animate-spin" />
          {t("onboarding.plugins.loading")}
        </div>
      ) : (
        <>
          {!available && (
            <p className="mt-5 text-xs text-zinc-500 dark:text-zinc-400">
              {t("onboarding.plugins.unavailable")}
            </p>
          )}
          <div className="mt-6 grid grid-cols-1 items-start gap-3 sm:grid-cols-2">
            {usages.map((usage) => (
              <PluginUsageCard
                key={usage}
                usage={usage}
                available={available}
                entries={entries.filter((e) => usageOf(e) === usage)}
                installs={installs}
                onInstall={(id) => {
                  onTouch();
                  void install(id);
                }}
              />
            ))}
          </div>
        </>
      )}
    </div>
  );
}

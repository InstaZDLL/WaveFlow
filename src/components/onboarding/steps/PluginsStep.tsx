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

/** A card's height, in rows: its scene and text, then one per plugin. */
const CARD_ROWS = 3;

/**
 * Two columns of cards, each card going to the shorter one in turn, so a
 * long group (the lyrics sources) does not leave a hole beside the short
 * ones. On a narrow window the grid has one column and the two stack.
 */
function balance<T>(items: readonly T[], rowsOf: (item: T) => number): T[][] {
  const columns: T[][] = [[], []];
  const heights = [0, 0];
  for (const item of items) {
    const shorter = heights[1] < heights[0] ? 1 : 0;
    columns[shorter].push(item);
    heights[shorter] += CARD_ROWS + rowsOf(item);
  }
  return columns.filter((column) => column.length > 0);
}

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
  const entriesOf = (usage: PluginUsage) =>
    entries.filter((e) => usageOf(e) === usage);

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
            {balance(usages, (usage) => entriesOf(usage).length).map(
              (column, i) => (
                <div key={i} className="grid min-w-0 grid-cols-1 gap-3">
                  {column.map((usage) => (
                    <PluginUsageCard
                      key={usage}
                      usage={usage}
                      available={available}
                      entries={entriesOf(usage)}
                      installs={installs}
                      onInstall={(id) => {
                        onTouch();
                        void install(id);
                      }}
                    />
                  ))}
                </div>
              ),
            )}
          </div>
        </>
      )}
    </div>
  );
}

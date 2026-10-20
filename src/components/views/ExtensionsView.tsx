import { useTranslation } from "react-i18next";
import { Puzzle } from "lucide-react";
import { PluginStoreCard } from "./settings/PluginStoreCard";
import { PluginsCard } from "./settings/PluginsCard";

/**
 * The extensions page: the plugin store and the installed plugins, as
 * a destination of their own in the sidebar rather than a category of
 * Settings. Most never went that far into Settings and so never saw
 * what the store holds; here it sits beside the entries the plugins
 * themselves add (Web Radio, Release Radar). The two cards are the
 * ones Settings used to host, unchanged, so a plugin's options, its
 * credentials and the store's install flow stay in one place.
 */
export function ExtensionsView() {
  const { t } = useTranslation();
  return (
    <div className="space-y-8 animate-fade-in pb-20">
      {/* Header — the same shape as the liked / history pages: a kicker,
          the title, one line. `data-page-hero` is the skins' hook. */}
      <div data-page-hero className="flex items-start justify-between">
        <div className="flex items-center space-x-6">
          <div className="w-24 h-24 rounded-2xl bg-violet-100 text-violet-500 flex items-center justify-center shadow-sm dark:bg-violet-950/60 dark:text-violet-400">
            <Puzzle size={48} />
          </div>
          <div>
            <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
              WaveFlow
            </div>
            <h1 className="text-4xl font-bold text-zinc-900 dark:text-white">
              {t("settings.organization.categories.plugins")}
            </h1>
            <div className="text-sm text-zinc-500 mt-1">
              {t("settings.organization.descriptions.plugins")}
            </div>
          </div>
        </div>
      </div>

      <div className="max-w-5xl space-y-6">
        <PluginStoreCard />
        <PluginsCard />
      </div>
    </div>
  );
}

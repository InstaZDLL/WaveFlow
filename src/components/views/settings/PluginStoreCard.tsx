import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Store,
  Globe,
  Database,
  Download,
  RefreshCw,
  Check,
  BadgeCheck,
} from "lucide-react";

import {
  listPluginMarketplace,
  installPluginFromRegistry,
  type MarketplaceEntry,
} from "../../../lib/tauri/plugins";
import { PLUGIN_AVAILABILITY_EVENT } from "../../../hooks/usePluginAvailability";

/**
 * Settings → Plugins → Store (Phase 2). Browses the curated registry
 * (`InstaZDLL/waveflow-plugins`) and installs / updates plugins in place.
 *
 * The backend fetches the catalogue (app endpoint → raw GitHub → jsDelivr
 * fallbacks), verifies each download's blake3 against the registry, and
 * stage-swaps the plugin into the sideload root — so a store install is
 * hash-pinned and sandboxed exactly like a hand-sideloaded one. This card
 * sits above {@link PluginsCard} (browse & install here, manage installed
 * ones below).
 *
 * A fetch error (offline mode, or no registry source reachable) surfaces
 * a retry affordance rather than an empty list, so "store unreachable"
 * never reads as "no plugins exist".
 */
export function PluginStoreCard() {
  const { t } = useTranslation();
  const [entries, setEntries] = useState<MarketplaceEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  // Manual (re)load — an event handler, so a synchronous `setLoading(true)`
  // is fine here (unlike the mount effect below, which must not setState
  // synchronously — see the `.then`-style guarded fetch in that effect).
  const refresh = useCallback(() => {
    setLoading(true);
    listPluginMarketplace().then(
      (list) => {
        setEntries(list);
        setError(null);
        setLoading(false);
      },
      (e: unknown) => {
        setError(e instanceof Error ? e.message : String(e));
        setLoading(false);
      },
    );
  }, []);

  useEffect(() => {
    // Same shape as PluginsCard: no synchronous setState in the effect
    // body (`react-hooks/set-state-in-effect`), setState only inside the
    // promise callbacks, `cancelled` guards a late resolve post-unmount.
    let cancelled = false;
    listPluginMarketplace().then(
      (list) => {
        if (cancelled) return;
        setEntries(list);
        setError(null);
        setLoading(false);
      },
      (e: unknown) => {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : String(e));
        setLoading(false);
      },
    );
    return () => {
      cancelled = true;
    };
  }, []);

  const onInstall = useCallback(
    async (entry: MarketplaceEntry) => {
      setBusyId(entry.id);
      setError(null);
      try {
        await installPluginFromRegistry(entry.id);
        // Reflect the new on-disk state (installed / version / no-update)
        // and let the sidebar + source views re-evaluate availability.
        window.dispatchEvent(new CustomEvent(PLUGIN_AVAILABILITY_EVENT));
        const list = await listPluginMarketplace();
        setEntries(list);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setBusyId(null);
      }
    },
    [],
  );

  return (
    <section
      aria-labelledby="settings-plugin-store-heading"
      className="bg-white dark:bg-zinc-900 rounded-2xl border border-zinc-200 dark:border-zinc-800 overflow-hidden mb-4"
    >
      <header className="px-4 py-3 border-b border-zinc-200 dark:border-zinc-800 flex items-start gap-3">
        <Store
          size={20}
          className="text-zinc-400 mt-0.5 shrink-0"
          aria-hidden="true"
        />
        <div className="min-w-0 flex-1">
          <h3
            id="settings-plugin-store-heading"
            className="text-sm font-medium text-zinc-900 dark:text-white"
          >
            {t("settings.pluginStore.title")}
          </h3>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed mt-0.5">
            {t("settings.pluginStore.subtitle")}
          </p>
        </div>
        <button
          type="button"
          onClick={() => refresh()}
          disabled={loading || busyId !== null}
          className="flex items-center gap-1 px-2 py-1 text-xs font-medium text-zinc-600 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded disabled:opacity-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 shrink-0"
          aria-label={t("settings.pluginStore.refresh")}
        >
          <RefreshCw
            size={13}
            aria-hidden="true"
            className={loading ? "animate-spin" : undefined}
          />
        </button>
      </header>

      {error && (
        <div
          role="alert"
          className="px-4 py-2 bg-amber-50 dark:bg-amber-950/30 text-xs text-amber-800 dark:text-amber-300 border-b border-amber-200 dark:border-amber-900 flex items-center justify-between gap-3"
        >
          <span className="min-w-0 truncate">{error}</span>
          <button
            type="button"
            onClick={() => refresh()}
            className="shrink-0 underline hover:no-underline focus:outline-none focus-visible:ring-2 focus-visible:ring-amber-500 rounded"
          >
            {t("settings.pluginStore.retry")}
          </button>
        </div>
      )}

      {loading ? (
        <div className="px-4 py-6 text-xs text-zinc-500 dark:text-zinc-400 text-center">
          {t("settings.pluginStore.loading")}
        </div>
      ) : entries.length === 0 && !error ? (
        <div className="px-4 py-8 text-center">
          <Store
            size={32}
            className="mx-auto text-zinc-300 dark:text-zinc-700 mb-2"
            aria-hidden="true"
          />
          <p className="text-sm text-zinc-600 dark:text-zinc-300">
            {t("settings.pluginStore.emptyTitle")}
          </p>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-1">
            {t("settings.pluginStore.emptyHint")}
          </p>
        </div>
      ) : (
        <ul className="divide-y divide-zinc-200 dark:divide-zinc-800">
          {entries.map((entry) => (
            <StoreRow
              key={entry.id}
              entry={entry}
              busy={busyId === entry.id}
              anyBusy={busyId !== null}
              onInstall={() => void onInstall(entry)}
            />
          ))}
        </ul>
      )}
    </section>
  );
}

function StoreRow({
  entry,
  busy,
  anyBusy,
  onInstall,
}: {
  entry: MarketplaceEntry;
  busy: boolean;
  anyBusy: boolean;
  onInstall: () => void;
}) {
  const { t } = useTranslation();
  return (
    <li className="px-4 py-3">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 flex-wrap">
            <span className="text-sm font-medium text-zinc-900 dark:text-white truncate">
              {entry.name}
            </span>
            <span className="text-xs text-zinc-500 dark:text-zinc-400">
              v{entry.version}
            </span>
            <WorldBadge world={entry.world} />
            {entry.official && (
              <span className="flex items-center gap-1 text-[10px] uppercase tracking-wide font-medium px-1.5 py-0.5 rounded bg-sky-50 text-sky-700 dark:bg-sky-950/30 dark:text-sky-400">
                <BadgeCheck size={11} aria-hidden="true" />
                {t("settings.pluginStore.official")}
              </span>
            )}
          </div>
          <div className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5 truncate">
            {t("settings.plugins.byAuthor", { author: entry.author })}
          </div>
          <p className="text-xs text-zinc-600 dark:text-zinc-300 mt-1 leading-relaxed">
            {entry.description}
          </p>
          <PermissionsPreview entry={entry} />
        </div>
        <div className="shrink-0 pt-0.5">
          <InstallButton
            entry={entry}
            busy={busy}
            anyBusy={anyBusy}
            onInstall={onInstall}
          />
        </div>
      </div>
    </li>
  );
}

function InstallButton({
  entry,
  busy,
  anyBusy,
  onInstall,
}: {
  entry: MarketplaceEntry;
  busy: boolean;
  anyBusy: boolean;
  onInstall: () => void;
}) {
  const { t } = useTranslation();

  if (!entry.compatible) {
    return (
      <span
        className="flex items-center gap-1 text-[11px] text-zinc-500 dark:text-zinc-400 italic"
        title={t("settings.pluginStore.incompatibleHint")}
      >
        {t("settings.pluginStore.incompatible")}
      </span>
    );
  }

  if (entry.installed && !entry.updateAvailable) {
    return (
      <span className="flex items-center gap-1 text-[11px] font-medium text-emerald-600 dark:text-emerald-400">
        <Check size={13} aria-hidden="true" />
        {t("settings.pluginStore.installed")}
      </span>
    );
  }

  const isUpdate = entry.installed && entry.updateAvailable;
  const label = busy
    ? t("settings.pluginStore.installing")
    : isUpdate
      ? t("settings.pluginStore.update")
      : t("settings.pluginStore.install");

  return (
    <button
      type="button"
      onClick={onInstall}
      disabled={anyBusy}
      className="flex items-center gap-1 px-2.5 py-1 text-xs font-medium text-white bg-emerald-600 hover:bg-emerald-700 rounded disabled:opacity-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
      aria-label={
        isUpdate
          ? t("settings.pluginStore.updateAria", { name: entry.name })
          : t("settings.pluginStore.installAria", { name: entry.name })
      }
    >
      {isUpdate ? (
        <RefreshCw
          size={13}
          aria-hidden="true"
          className={busy ? "animate-spin" : undefined}
        />
      ) : (
        <Download
          size={13}
          aria-hidden="true"
          className={busy ? "animate-pulse" : undefined}
        />
      )}
      <span>{label}</span>
    </button>
  );
}

function WorldBadge({ world }: { world: string }) {
  const { t } = useTranslation();
  const label = world.startsWith("waveflow:source")
    ? t("settings.plugins.worlds.source")
    : world.startsWith("waveflow:metadata")
      ? t("settings.plugins.worlds.metadata")
      : world.startsWith("waveflow:ui")
        ? t("settings.plugins.worlds.ui")
        : world;
  return (
    <span className="text-[10px] uppercase tracking-wide font-medium px-1.5 py-0.5 rounded bg-emerald-50 text-emerald-700 dark:bg-emerald-950/30 dark:text-emerald-400">
      {label}
    </span>
  );
}

/**
 * The permission ask, shown BEFORE install so the user sees exactly what
 * the plugin can reach. The host enforces this allowlist at runtime, so
 * it's a real contract, not a label — we list the actual hostnames.
 */
function PermissionsPreview({ entry }: { entry: MarketplaceEntry }) {
  const { t } = useTranslation();
  if (entry.http.length === 0 && !entry.storageState) {
    return (
      <p className="text-[11px] text-zinc-400 dark:text-zinc-500 mt-1.5 italic">
        {t("settings.plugins.permissions.none")}
      </p>
    );
  }
  return (
    <div className="mt-1.5">
      {entry.http.length > 0 && (
        <div className="flex items-start gap-1.5">
          <Globe
            size={11}
            className="text-zinc-400 mt-1 shrink-0"
            aria-hidden="true"
          />
          <div className="min-w-0">
            <span className="text-[11px] text-zinc-500 dark:text-zinc-400">
              {t("settings.pluginStore.canReach")}
            </span>
            <ul className="inline-flex flex-wrap gap-1 ml-1 align-middle">
              {entry.http.map((host) => (
                <li
                  key={host}
                  className="text-[11px] px-1.5 py-0.5 rounded bg-zinc-100 text-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 font-mono"
                >
                  {host}
                </li>
              ))}
            </ul>
          </div>
        </div>
      )}
      {entry.storageState && (
        <div className="flex items-center gap-1 mt-1 text-[11px] text-zinc-500 dark:text-zinc-400">
          <Database size={11} aria-hidden="true" />
          <span>{t("settings.plugins.permissions.storageState")}</span>
        </div>
      )}
    </div>
  );
}

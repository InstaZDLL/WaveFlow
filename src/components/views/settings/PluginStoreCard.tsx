import { useCallback, useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Store,
  Globe,
  Database,
  FileText,
  Download,
  RefreshCw,
  Check,
  BadgeCheck,
  Users,
  Loader2,
  Search,
  X,
} from "lucide-react";

import {
  listPluginMarketplace,
  installPluginFromRegistry,
  type MarketplaceEntry,
} from "../../../lib/tauri/plugins";
import { PLUGIN_AVAILABILITY_EVENT } from "../../../hooks/usePluginAvailability";
import { useLocalizedText } from "../../../hooks/useLocalizedText";
import { pluginWorldLabelKey } from "../../../lib/pluginWorlds";
import { foldForSearch, searchTerms } from "../../../lib/searchFold";
import { PluginWorldBadge } from "./PluginWorldBadge";

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
/** How the store lays itself out: the settings card it was born as,
 *  or a grid of tiles for the Extensions page, where the entries that
 *  are already installed are left to the installed list below. */
export type PluginListVariant = "card" | "grid";

export function PluginStoreCard({
  variant = "card",
}: {
  variant?: PluginListVariant;
} = {}) {
  const grid = variant === "grid";
  const { t } = useTranslation();
  const [entries, setEntries] = useState<MarketplaceEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const searchId = useId();
  const searchRef = useRef<HTMLInputElement>(null);
  const localize = useLocalizedText();

  // One read path for the mount, the availability event and the manual
  // refresh, behind one sequence: whichever of them answers last cannot
  // land an older snapshot over a newer state (an installed entry back
  // on offer, say). The sequence also outlives the effect, so a read
  // that answers after the unmount writes nothing.
  const seqRef = useRef(0);
  const load = useCallback(() => {
    const mine = ++seqRef.current;
    listPluginMarketplace().then(
      (list) => {
        if (mine !== seqRef.current) return;
        setEntries(list);
        setError(null);
        setLoading(false);
      },
      (e: unknown) => {
        if (mine !== seqRef.current) return;
        setError(e instanceof Error ? e.message : String(e));
        setLoading(false);
      },
    );
  }, []);

  // The manual button: an event handler, so a synchronous
  // `setLoading(true)` is fine here (the mount effect below must not
  // setState synchronously — `react-hooks/set-state-in-effect`).
  const refresh = useCallback(() => {
    setLoading(true);
    load();
  }, [load]);

  useEffect(() => {
    load();
    // An uninstall or a toggle in the installed list beside this store
    // dispatches the event: re-read, so the entry comes back on offer.
    window.addEventListener(PLUGIN_AVAILABILITY_EVENT, load);
    // The ref is a counter, not a node: advancing it at cleanup is the
    // point, and the alias keeps the lint's node-ref heuristic quiet.
    const counter = seqRef;
    return () => {
      window.removeEventListener(PLUGIN_AVAILABILITY_EVENT, load);
      counter.current += 1;
    };
  }, [load]);

  const onInstall = useCallback(async (entry: MarketplaceEntry) => {
    setBusyId(entry.id);
    setError(null);
    try {
      await installPluginFromRegistry(entry.id);
      // Reflect the new on-disk state (installed / version / no-update)
      // and let the sidebar, the source views and the installed list
      // beside this store re-evaluate availability; this store re-reads
      // on the same event.
      window.dispatchEvent(new CustomEvent(PLUGIN_AVAILABILITY_EVENT));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusyId(null);
    }
  }, []);

  // On the page, an entry already on disk belongs to the installed list
  // beneath; the store shows what is left to get, or to update.
  const available = grid
    ? entries.filter((e) => !e.installed || e.updateAvailable)
    : entries;

  // The search runs over the catalogue already loaded, on what the row
  // shows in the user's language: the name (registries carry it as is),
  // the description resolved like the row resolves it plus its English
  // source (a term is often typed in English whatever the UI language),
  // the world chip's label beside the raw WIT world, the author and the
  // registry tags. Every word of the query must appear, in any order.
  const terms = searchTerms(query);
  const shown = terms.length
    ? available.filter((entry) => {
        const worldKey = pluginWorldLabelKey(entry.world);
        const english =
          entry.description && typeof entry.description === "object"
            ? (entry.description.en ?? "")
            : "";
        const text = foldForSearch(
          [
            entry.name,
            localize(entry.description) ?? "",
            english,
            worldKey ? t(worldKey) : "",
            entry.world,
            entry.author,
            ...entry.tags,
          ].join(" "),
        );
        return terms.every((term) => text.includes(term));
      })
    : available;

  return (
    <section
      aria-labelledby="settings-plugin-store-heading"
      className={
        grid
          ? "space-y-3"
          : "bg-white dark:bg-zinc-900 rounded-2xl border border-zinc-200 dark:border-zinc-800 overflow-hidden mb-4"
      }
    >
      <header
        className={
          grid
            ? "flex items-start gap-3 px-1"
            : "px-4 py-3 border-b border-zinc-200 dark:border-zinc-800 flex items-start gap-3"
        }
      >
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
          <p className="text-xs mt-0.5 settings-description">
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
          {loading ? (
            <Loader2 size={13} aria-hidden="true" className="animate-spin" />
          ) : (
            <RefreshCw size={13} aria-hidden="true" />
          )}
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

      {!loading && available.length > 0 && (
        <div className={grid ? "relative max-w-md" : "relative mx-4 mt-3"}>
          <label htmlFor={searchId} className="sr-only">
            {t("settings.pluginStore.search")}
          </label>
          <Search
            size={14}
            aria-hidden="true"
            className="pointer-events-none absolute start-2.5 top-1/2 -translate-y-1/2 text-zinc-400"
          />
          <input
            ref={searchRef}
            id={searchId}
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape" && query) {
                event.stopPropagation();
                setQuery("");
              }
            }}
            placeholder={t("settings.pluginStore.search")}
            className="h-8 w-full rounded-lg border border-zinc-200 bg-white ps-8 pe-8 text-xs text-zinc-800 outline-none focus:border-emerald-500 focus:ring-2 focus:ring-emerald-500/20 dark:border-zinc-700 dark:bg-zinc-900 dark:text-zinc-100 [&::-webkit-search-cancel-button]:appearance-none"
          />
          {query && (
            <button
              type="button"
              onClick={() => {
                setQuery("");
                // The button goes with the query; focus stays in the field
                // rather than falling to the page.
                searchRef.current?.focus();
              }}
              aria-label={t("settings.pluginStore.clearSearch")}
              className="absolute end-1.5 top-1/2 -translate-y-1/2 rounded-md p-1 text-zinc-500 hover:bg-zinc-100 focus-visible:outline-emerald-500 dark:hover:bg-zinc-800"
            >
              <X size={13} aria-hidden="true" />
            </button>
          )}
        </div>
      )}

      {loading ? (
        <div className="px-4 py-6 text-xs text-zinc-500 dark:text-zinc-400 text-center">
          {t("settings.pluginStore.loading")}
        </div>
      ) : grid && available.length === 0 && entries.length > 0 ? (
        <p className="px-1 py-2 text-xs text-zinc-500 dark:text-zinc-400">
          {t("settings.pluginStore.allInstalled")}
        </p>
      ) : terms.length > 0 && shown.length === 0 ? (
        <p
          role="status"
          className={
            grid
              ? "px-1 py-2 text-xs text-zinc-500 dark:text-zinc-400"
              : "px-4 py-6 text-xs text-zinc-500 dark:text-zinc-400 text-center"
          }
        >
          {t("settings.pluginStore.noMatch", { query: query.trim() })}
        </p>
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
        <ul
          className={
            grid
              ? "grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3"
              : "divide-y divide-zinc-200 dark:divide-zinc-800"
          }
        >
          {shown.map((entry) => (
            <StoreRow
              key={entry.id}
              entry={entry}
              tile={grid}
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
  tile,
  busy,
  anyBusy,
  onInstall,
}: {
  entry: MarketplaceEntry;
  /** A tile of the page's grid: framed, the description clamped, the
   *  permissions folded behind a summary. */
  tile: boolean;
  busy: boolean;
  anyBusy: boolean;
  onInstall: () => void;
}) {
  const { t } = useTranslation();
  // Registry descriptions are authored in the catalogue, not in our
  // i18next files — they carry their own language map. A registry comes
  // off the network, so an unusable value degrades to no paragraph
  // rather than an empty one holding its margin.
  const description = useLocalizedText()(entry.description);
  return (
    <li
      className={
        tile
          ? "flex flex-col rounded-2xl border border-zinc-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900"
          : "px-4 py-3"
      }
    >
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 flex-wrap">
            <span className="text-sm font-medium text-zinc-900 dark:text-white truncate">
              {entry.name}
            </span>
            <span className="text-xs text-zinc-500 dark:text-zinc-400">
              v{entry.version}
            </span>
            <PluginWorldBadge world={entry.world} />
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
          {description && (
            <p
              className={`text-xs text-zinc-600 dark:text-zinc-300 mt-1 leading-relaxed ${tile ? "line-clamp-2" : ""}`}
            >
              {description}
            </p>
          )}
          {tile ? (
            <details className="mt-1.5">
              <summary className="cursor-pointer select-none text-[11px] text-zinc-500 dark:text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200">
                {t("settings.plugins.permissions.summary")}
              </summary>
              <PermissionsPreview entry={entry} />
            </details>
          ) : (
            <PermissionsPreview entry={entry} />
          )}
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
      {isUpdate && busy ? (
        <Loader2 size={13} aria-hidden="true" className="animate-spin" />
      ) : isUpdate ? (
        <RefreshCw size={13} aria-hidden="true" />
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

/**
 * The permission ask, shown BEFORE install so the user sees exactly what
 * the plugin can reach. The host enforces this allowlist at runtime, so
 * it's a real contract, not a label — we list the actual hostnames.
 */
function PermissionsPreview({ entry }: { entry: MarketplaceEntry }) {
  const { t } = useTranslation();
  if (
    entry.http.length === 0 &&
    !entry.storageState &&
    !entry.storageRead &&
    !entry.libraryReadArtists
  ) {
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
      {entry.storageRead && (
        <div className="flex items-center gap-1 mt-1 text-[11px] text-zinc-500 dark:text-zinc-400">
          <FileText size={11} aria-hidden="true" />
          <span>{t("settings.plugins.permissions.storageRead")}</span>
        </div>
      )}
      {entry.libraryReadArtists && (
        <div className="flex items-center gap-1 mt-1 text-[11px] text-zinc-500 dark:text-zinc-400">
          <Users size={11} aria-hidden="true" />
          <span>{t("settings.plugins.permissions.libraryReadArtists")}</span>
        </div>
      )}
    </div>
  );
}

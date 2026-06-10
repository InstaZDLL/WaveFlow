import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Puzzle, Trash2, Globe, Database, FileText } from "lucide-react";

import {
  listInstalledPlugins,
  setPluginEnabled,
  uninstallPlugin,
  type PluginInfo,
} from "../../../lib/tauri/plugins";

/**
 * Settings → Plugins panel (Phase 3.2). Lists every plugin
 * installed under `<app-data>/waveflow/plugins/`, surfaces its
 * manifest metadata + permissions, and exposes the enable toggle +
 * uninstall flow.
 *
 * Empty state is the normal case for v1.5.0 because the only
 * supported install path is sideload (drop a directory under
 * `<app-data>/waveflow/plugins/<id>/`). Phase 4 ships Web Radio
 * pre-installed; an official store + the install flow land
 * post-1.5.0.
 *
 * Uninstall uses an inline confirm (a second click on the same
 * row's button) rather than a modal — Settings is already a
 * focused surface, a modal would feel heavy for a per-row action.
 */
export function PluginsCard() {
  const { t } = useTranslation();
  const [plugins, setPlugins] = useState<PluginInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [confirmingUninstall, setConfirmingUninstall] = useState<string | null>(
    null,
  );
  const [busyId, setBusyId] = useState<string | null>(null);

  useEffect(() => {
    // `.then`-style instead of an `await` inside an IIFE because
    // `react-hooks/set-state-in-effect` traces synchronous
    // setState calls through awaits + IIFEs, and reads `.then`
    // callbacks as the external-subscription shape the rule
    // accepts. `cancelled` guards against a stale callback
    // landing setState after the component unmounted.
    let cancelled = false;
    listInstalledPlugins().then(
      (list) => {
        if (cancelled) return;
        setPlugins(list);
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

  const onToggle = useCallback(
    async (plugin: PluginInfo) => {
      setBusyId(plugin.id);
      // Optimistic flip — revert on backend error.
      setPlugins((prev) =>
        prev.map((p) =>
          p.id === plugin.id ? { ...p, enabled: !p.enabled } : p,
        ),
      );
      try {
        await setPluginEnabled(plugin.id, !plugin.enabled);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
        setPlugins((prev) =>
          prev.map((p) =>
            p.id === plugin.id ? { ...p, enabled: plugin.enabled } : p,
          ),
        );
      } finally {
        setBusyId(null);
      }
    },
    [],
  );

  const onUninstall = useCallback(
    async (pluginId: string) => {
      setBusyId(pluginId);
      try {
        await uninstallPlugin(pluginId);
        setPlugins((prev) => prev.filter((p) => p.id !== pluginId));
        setConfirmingUninstall(null);
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
      aria-labelledby="settings-plugins-heading"
      className="bg-white dark:bg-zinc-900 rounded-2xl border border-zinc-200 dark:border-zinc-800 overflow-hidden"
    >
      <header className="px-4 py-3 border-b border-zinc-200 dark:border-zinc-800 flex items-start gap-3">
        <Puzzle
          size={20}
          className="text-zinc-400 mt-0.5 shrink-0"
          aria-hidden="true"
        />
        <div className="min-w-0">
          <h3
            id="settings-plugins-heading"
            className="text-sm font-medium text-zinc-900 dark:text-white"
          >
            {t("settings.plugins.title")}
          </h3>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed mt-0.5">
            {t("settings.plugins.subtitle")}
          </p>
        </div>
      </header>

      {error && (
        <div
          role="alert"
          className="px-4 py-2 bg-red-50 dark:bg-red-950/30 text-xs text-red-700 dark:text-red-300 border-b border-red-200 dark:border-red-900"
        >
          {error}
        </div>
      )}

      {loading ? (
        <div className="px-4 py-6 text-xs text-zinc-500 dark:text-zinc-400 text-center">
          {t("settings.plugins.loading")}
        </div>
      ) : plugins.length === 0 ? (
        <div className="px-4 py-8 text-center">
          <Puzzle
            size={32}
            className="mx-auto text-zinc-300 dark:text-zinc-700 mb-2"
            aria-hidden="true"
          />
          <p className="text-sm text-zinc-600 dark:text-zinc-300">
            {t("settings.plugins.emptyTitle")}
          </p>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-1">
            {t("settings.plugins.emptyHint")}
          </p>
        </div>
      ) : (
        <ul className="divide-y divide-zinc-200 dark:divide-zinc-800">
          {plugins.map((plugin) => {
            const isBusy = busyId === plugin.id;
            const isConfirming = confirmingUninstall === plugin.id;
            return (
              <li key={plugin.id} className="px-4 py-3">
                <div className="flex items-start justify-between gap-3">
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2 flex-wrap">
                      <span className="text-sm font-medium text-zinc-900 dark:text-white truncate">
                        {plugin.name}
                      </span>
                      <span className="text-xs text-zinc-500 dark:text-zinc-400">
                        v{plugin.version}
                      </span>
                      <WorldBadge world={plugin.world} />
                    </div>
                    <div className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5 truncate">
                      {t("settings.plugins.byAuthor", { author: plugin.author })}
                    </div>
                    {plugin.description && (
                      <p className="text-xs text-zinc-600 dark:text-zinc-300 mt-1 leading-relaxed">
                        {plugin.description}
                      </p>
                    )}
                    <PermissionsRow
                      permissions={plugin.permissions}
                      assetsCount={plugin.assets.length}
                    />
                  </div>
                  <div className="flex flex-col items-end gap-2 shrink-0">
                    <label className="flex items-center gap-2 cursor-pointer">
                      <span className="text-xs text-zinc-500 dark:text-zinc-400">
                        {plugin.enabled
                          ? t("settings.plugins.enabled")
                          : t("settings.plugins.disabled")}
                      </span>
                      <input
                        type="checkbox"
                        checked={plugin.enabled}
                        disabled={isBusy}
                        onChange={() => {
                          void onToggle(plugin);
                        }}
                        className="w-4 h-4 accent-emerald-500 cursor-pointer disabled:opacity-50"
                        aria-label={t("settings.plugins.toggleAria", {
                          name: plugin.name,
                        })}
                      />
                    </label>
                    {isConfirming ? (
                      <div
                        role="group"
                        aria-label={t("settings.plugins.confirmUninstall", {
                          name: plugin.name,
                        })}
                        className="flex items-center gap-1"
                      >
                        <button
                          type="button"
                          onClick={() => {
                            void onUninstall(plugin.id);
                          }}
                          disabled={isBusy}
                          className="px-2 py-1 text-xs font-medium text-white bg-red-600 hover:bg-red-700 rounded disabled:opacity-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-red-500"
                        >
                          {t("settings.plugins.confirmYes")}
                        </button>
                        <button
                          type="button"
                          onClick={() => setConfirmingUninstall(null)}
                          disabled={isBusy}
                          className="px-2 py-1 text-xs font-medium text-zinc-700 dark:text-zinc-200 bg-zinc-100 hover:bg-zinc-200 dark:bg-zinc-800 dark:hover:bg-zinc-700 rounded disabled:opacity-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
                        >
                          {t("settings.plugins.confirmNo")}
                        </button>
                      </div>
                    ) : (
                      <button
                        type="button"
                        onClick={() => setConfirmingUninstall(plugin.id)}
                        disabled={isBusy}
                        className="flex items-center gap-1 px-2 py-1 text-xs font-medium text-red-600 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-950/30 rounded disabled:opacity-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-red-500"
                        aria-label={t("settings.plugins.uninstallAria", {
                          name: plugin.name,
                        })}
                      >
                        <Trash2 size={14} aria-hidden="true" />
                        <span>{t("settings.plugins.uninstall")}</span>
                      </button>
                    )}
                  </div>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

function WorldBadge({ world }: { world: string }) {
  const { t } = useTranslation();
  // Map the WIT world label to a short user-facing role.
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

function PermissionsRow({
  permissions,
  assetsCount,
}: {
  permissions: PluginInfo["permissions"];
  assetsCount: number;
}) {
  const { t } = useTranslation();
  const chips: { key: string; icon: typeof Globe; label: string }[] = [];
  if (permissions.http.length > 0) {
    chips.push({
      key: "http",
      icon: Globe,
      label: t("settings.plugins.permissions.http", {
        count: permissions.http.length,
      }),
    });
  }
  if (permissions.storageState) {
    chips.push({
      key: "state",
      icon: Database,
      label: t("settings.plugins.permissions.storageState"),
    });
  }
  // `storage.read` (permission to read bundled assets) and the
  // declared asset count are independent surfaces — a plugin can
  // request the permission without shipping assets (forward-compat)
  // and a plugin can ship assets without asking for the
  // permission (manifest oversight). Surface them as separate
  // chips so the user sees the truth on each axis instead of an
  // "Assets (0)" chip when only the permission is set.
  if (permissions.storageRead) {
    chips.push({
      key: "storageRead",
      icon: FileText,
      label: t("settings.plugins.permissions.storageRead"),
    });
  }
  if (assetsCount > 0) {
    chips.push({
      key: "assets",
      icon: FileText,
      label: t("settings.plugins.permissions.assets", { count: assetsCount }),
    });
  }
  if (chips.length === 0) {
    return (
      <p className="text-[11px] text-zinc-400 dark:text-zinc-500 mt-1.5 italic">
        {t("settings.plugins.permissions.none")}
      </p>
    );
  }
  return (
    <ul className="flex flex-wrap gap-1.5 mt-1.5">
      {chips.map(({ key, icon: Icon, label }) => (
        <li
          key={key}
          className="flex items-center gap-1 text-[11px] px-1.5 py-0.5 rounded bg-zinc-100 text-zinc-700 dark:bg-zinc-800 dark:text-zinc-300"
        >
          <Icon size={11} aria-hidden="true" />
          <span>{label}</span>
        </li>
      ))}
    </ul>
  );
}

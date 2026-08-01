import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { openUrl } from "@tauri-apps/plugin-opener";
import { AlertCircle, ExternalLink, Loader2, RefreshCw } from "lucide-react";

import {
  pluginUiEvent,
  pluginUiRender,
  type PluginUiAction,
  type PluginUiDescriptor,
  type PluginUiItem,
} from "../../lib/tauri/plugins";
import { resolvePluginIcon } from "../../lib/pluginIcons";

interface PluginUIViewProps {
  pluginId: string;
  /** Landing path from the plugin's manifest; defaults to `"/"`. */
  initialPath?: string;
  /** lucide icon name from the manifest, reused for the header glyph
   *  + image-less card placeholders. */
  icon?: string | null;
}

/**
 * Generic renderer for a `ui`-world plugin's JSON view descriptor. The
 * plugin never ships React — it returns a declarative tree (sections /
 * cards / images / action buttons) that this component draws with
 * native components. A user action round-trips through the plugin
 * (`event`) and swaps in the next descriptor, or opens an external URL
 * (`open-url`) host-side without touching the plugin.
 *
 * Deliberately generic: it respects the descriptor's section titles and
 * makes no assumptions about a specific plugin's semantics (no baked-in
 * icon-per-action-kind beyond the neutral open-url external-link glyph).
 */
export function PluginUIView({ pluginId, initialPath, icon }: PluginUIViewProps) {
  const { t } = useTranslation();
  const [descriptor, setDescriptor] = useState<PluginUiDescriptor | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  // Monotonic token: a stale render/event resolving after a pluginId
  // switch (or a newer action) must not clobber the current view.
  const reqRef = useRef(0);

  const load = useCallback(
    (path: string) => {
      const token = ++reqRef.current;
      setIsLoading(true);
      setError(null);
      pluginUiRender(pluginId, path).then(
        (d) => {
          if (token !== reqRef.current) return;
          setDescriptor(d);
          setIsLoading(false);
        },
        (e) => {
          if (token !== reqRef.current) return;
          setError(String(e));
          setIsLoading(false);
        },
      );
    },
    [pluginId],
  );

  useEffect(() => {
    // `load` flips isLoading/error synchronously before its async
    // fetch; that's the intended "start a request" pattern, not a
    // cascading-render bug (the view also remounts per plugin via the
    // `key` in AppLayout, so this runs once per plugin).
    // eslint-disable-next-line react-hooks/set-state-in-effect
    load(initialPath ?? "/");
  }, [load, initialPath]);

  const runAction = useCallback(
    (action: PluginUiAction, actionKey: string) => {
      if (action.kind === "open-url") {
        if (action.url) {
          openUrl(action.url).catch((e) =>
            console.error("[PluginUIView] openUrl failed", e),
          );
        }
        return;
      }
      // event → round-trip to the plugin → replace the whole view.
      const token = ++reqRef.current;
      setBusyAction(actionKey);
      pluginUiEvent(pluginId, action.event ?? "", action.payload ?? "").then(
        (d) => {
          if (token !== reqRef.current) return;
          setDescriptor(d);
          setBusyAction(null);
        },
        (e) => {
          if (token !== reqRef.current) return;
          setError(String(e));
          setBusyAction(null);
        },
      );
    },
    [pluginId],
  );

  const renderAction = (action: PluginUiAction, actionKey: string) => {
    const busy = busyAction === actionKey;
    return (
      <button
        key={actionKey}
        type="button"
        disabled={busy}
        onClick={() => runAction(action, actionKey)}
        className="inline-flex items-center gap-1.5 rounded-full border border-zinc-200 dark:border-zinc-700 px-3 py-1 text-xs font-medium text-zinc-700 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 disabled:opacity-50 transition-colors"
      >
        {busy ? (
          <Loader2 size={13} className="animate-spin" />
        ) : action.kind === "open-url" ? (
          <ExternalLink size={13} />
        ) : null}
        {action.label}
      </button>
    );
  };

  const renderItem = (item: PluginUiItem, key: string) => (
    <div
      key={key}
      className="flex items-center gap-3 rounded-xl border border-zinc-100 dark:border-zinc-800 bg-white/60 dark:bg-zinc-900/40 p-3"
    >
      <div className="h-14 w-14 shrink-0 overflow-hidden rounded-lg bg-zinc-100 dark:bg-zinc-800 flex items-center justify-center text-zinc-400">
        {item.imageUrl ? (
          <img
            src={item.imageUrl}
            alt=""
            className="h-full w-full object-cover"
            loading="lazy"
          />
        ) : (
          resolvePluginIcon(icon, 22)
        )}
      </div>
      <div className="min-w-0 flex-1">
        <p className="truncate font-medium text-zinc-900 dark:text-zinc-100">
          {item.title}
        </p>
        {item.subtitle && (
          <p className="truncate text-sm text-zinc-500 dark:text-zinc-400">
            {item.subtitle}
          </p>
        )}
        {item.badges && item.badges.length > 0 && (
          <div className="mt-1 flex flex-wrap gap-1">
            {item.badges.map((badge, i) => (
              <span
                key={i}
                className="rounded-full bg-zinc-100 dark:bg-zinc-800 px-2 py-0.5 text-[11px] text-zinc-600 dark:text-zinc-300"
              >
                {badge}
              </span>
            ))}
          </div>
        )}
      </div>
      {item.detail && (
        <span className="shrink-0 text-sm text-zinc-400 dark:text-zinc-500">
          {item.detail}
        </span>
      )}
      {item.actions && item.actions.length > 0 && (
        <div className="flex shrink-0 items-center gap-1.5">
          {item.actions.map((action, i) =>
            renderAction(action, `${key}:action:${i}`),
          )}
        </div>
      )}
    </div>
  );

  const sections = descriptor?.sections ?? [];
  const hasItems = sections.some((s) => s.items.length > 0);

  return (
    <div className="mx-auto flex w-full flex-col gap-6 p-6">
      {/* Header */}
      <header className="flex items-start gap-3">
        <span className="mt-1 text-emerald-500">{resolvePluginIcon(icon, 24)}</span>
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-2xl font-bold text-zinc-900 dark:text-zinc-100">
            {descriptor?.title ?? ""}
          </h1>
          {descriptor?.subtitle && (
            <p className="truncate text-sm text-zinc-500 dark:text-zinc-400">
              {descriptor.subtitle}
            </p>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-1.5">
          {descriptor?.actions?.map((action, i) =>
            renderAction(action, `header:action:${i}`),
          )}
          <button
            type="button"
            disabled={isLoading}
            onClick={() => load(initialPath ?? "/")}
            className="inline-flex items-center gap-1.5 rounded-full border border-zinc-200 dark:border-zinc-700 px-3 py-1 text-xs font-medium text-zinc-700 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 disabled:opacity-50 transition-colors"
          >
            {isLoading ? (
              <Loader2 size={13} className="animate-spin" />
            ) : (
              <RefreshCw size={13} />
            )}
            {t("pluginView.refresh")}
          </button>
        </div>
      </header>

      {/* Error banner — non-destructive: keeps the last good view below. */}
      {error && (
        <div className="flex items-start gap-2 rounded-xl border border-red-200 dark:border-red-900/50 bg-red-50 dark:bg-red-950/30 p-3 text-sm text-red-700 dark:text-red-300">
          <AlertCircle size={16} className="mt-0.5 shrink-0" />
          <div className="flex-1">
            <p className="font-medium">{t("pluginView.error")}</p>
            <p className="text-red-600/80 dark:text-red-400/80">{error}</p>
          </div>
          <button
            type="button"
            disabled={isLoading}
            onClick={() => load(initialPath ?? "/")}
            className="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-red-300 dark:border-red-800 px-3 py-1 text-xs font-medium hover:bg-red-100 dark:hover:bg-red-900/40 disabled:opacity-50 transition-colors"
          >
            {isLoading && <Loader2 size={13} className="animate-spin" />}
            {t("pluginView.retry")}
          </button>
        </div>
      )}

      {/* Body */}
      {isLoading && !descriptor ? (
        <div className="flex items-center justify-center gap-2 py-16 text-zinc-400">
          <Loader2 size={18} className="animate-spin" />
          {t("common.loading")}
        </div>
      ) : !hasItems ? (
        <div className="flex flex-col items-center gap-2 py-16 text-center text-zinc-400">
          <span>{resolvePluginIcon(icon, 32)}</span>
          <p className="font-medium text-zinc-600 dark:text-zinc-300">
            {descriptor?.emptyTitle ?? t("pluginView.empty")}
          </p>
          {descriptor?.emptyHint && (
            <p className="text-sm">{descriptor.emptyHint}</p>
          )}
        </div>
      ) : (
        <div className="flex flex-col gap-6">
          {sections.map((section, si) =>
            section.items.length === 0 ? null : (
              <section key={si} className="flex flex-col gap-2">
                {section.title && (
                  <h2 className="text-sm font-semibold uppercase tracking-wide text-zinc-500 dark:text-zinc-400">
                    {section.title}
                  </h2>
                )}
                <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
                  {section.items.map((item, ii) =>
                    renderItem(item, `${si}:${ii}:${item.id}`),
                  )}
                </div>
              </section>
            ),
          )}
        </div>
      )}
    </div>
  );
}

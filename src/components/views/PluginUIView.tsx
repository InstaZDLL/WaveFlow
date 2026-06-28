import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ExternalLink, Loader2, Radar, RefreshCw, X } from "lucide-react";
import {
  pluginUiEvent,
  pluginUiRender,
  type PluginUiAction,
  type PluginUiDescriptor,
} from "../../lib/tauri/plugins";

interface PluginUIViewProps {
  pluginId: string;
  initialPath?: string;
}

export function PluginUIView({ pluginId, initialPath = "/" }: PluginUIViewProps) {
  const [descriptor, setDescriptor] = useState<PluginUiDescriptor | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    pluginUiRender(pluginId, initialPath).then(
      (next) => {
        if (cancelled) return;
        setDescriptor(next);
        setError(null);
        setIsLoading(false);
      },
      (err) => {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : String(err));
        setIsLoading(false);
      },
    );
    return () => {
      cancelled = true;
    };
  }, [pluginId, initialPath]);

  const runAction = async (action: PluginUiAction) => {
    if (action.kind === "open-url" && action.url) {
      await openUrl(action.url);
      return;
    }
    if (action.kind !== "event" || !action.event) return;
    setBusyAction(`${action.event}:${action.payload ?? ""}`);
    setError(null);
    try {
      setDescriptor(
        await pluginUiEvent(pluginId, action.event, action.payload ?? ""),
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyAction(null);
    }
  };

  const items =
    descriptor?.sections?.flatMap((section) =>
      section.items.map((item) => ({ section: section.title, item })),
    ) ?? [];

  return (
    <div className="h-full overflow-y-auto px-6 py-6">
      <div className="mx-auto max-w-6xl space-y-5">
        <header className="flex flex-wrap items-center gap-3 border-b border-zinc-200 pb-5 dark:border-zinc-800">
          <div className="flex h-10 w-10 items-center justify-center rounded-lg bg-emerald-100 text-emerald-700 dark:bg-emerald-950/50 dark:text-emerald-300">
            <Radar size={20} />
          </div>
          <div className="min-w-0 flex-1">
            <h1 className="text-2xl font-semibold text-zinc-900 dark:text-white">
              {descriptor?.title ?? "Plugin"}
            </h1>
            {descriptor?.subtitle && (
              <p className="mt-1 text-sm text-zinc-500 dark:text-zinc-400">
                {descriptor.subtitle}
              </p>
            )}
          </div>
          <div className="flex flex-wrap items-center gap-2">
            {(descriptor?.actions ?? []).map((action) => (
              <button
                key={`${action.kind}:${action.event ?? action.url}:${action.label}`}
                type="button"
                onClick={() => runAction(action)}
                disabled={busyAction != null}
                className="inline-flex items-center gap-2 rounded-lg border border-zinc-200 px-3 py-2 text-sm font-medium text-zinc-700 transition-colors hover:bg-zinc-100 disabled:opacity-50 dark:border-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-800"
              >
                {busyAction?.startsWith(action.event ?? "") ? (
                  <Loader2 size={15} className="animate-spin" />
                ) : (
                  <RefreshCw size={15} />
                )}
                {action.label}
              </button>
            ))}
          </div>
        </header>

        {error && (
          <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
            {error}
          </div>
        )}

        {isLoading ? (
          <div className="flex h-56 items-center justify-center text-zinc-500">
            <Loader2 size={24} className="animate-spin" />
          </div>
        ) : items.length === 0 ? (
          <div className="rounded-lg border border-dashed border-zinc-300 px-6 py-12 text-center dark:border-zinc-700">
            <p className="text-base font-medium text-zinc-800 dark:text-zinc-100">
              {descriptor?.emptyTitle ?? "Nothing to show"}
            </p>
            <p className="mt-2 text-sm text-zinc-500 dark:text-zinc-400">
              {descriptor?.emptyHint ?? ""}
            </p>
          </div>
        ) : (
          <div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
            {items.map(({ item }) => (
              <article
                key={item.id}
                className="flex min-w-0 gap-4 rounded-lg border border-zinc-200 bg-white/70 p-3 dark:border-zinc-800 dark:bg-zinc-900/50"
              >
                <div className="h-20 w-20 flex-shrink-0 overflow-hidden rounded-md bg-zinc-100 dark:bg-zinc-800">
                  {item.imageUrl ? (
                    <img
                      src={item.imageUrl}
                      alt=""
                      className="h-full w-full object-cover"
                      loading="lazy"
                    />
                  ) : (
                    <div className="flex h-full w-full items-center justify-center text-zinc-400">
                      <Radar size={24} />
                    </div>
                  )}
                </div>
                <div className="min-w-0 flex-1">
                  <div className="flex min-w-0 items-start gap-2">
                    <div className="min-w-0 flex-1">
                      <h2 className="truncate text-sm font-semibold text-zinc-900 dark:text-white">
                        {item.title}
                      </h2>
                      <p className="mt-1 truncate text-sm text-zinc-500 dark:text-zinc-400">
                        {item.subtitle}
                      </p>
                    </div>
                    {item.detail && (
                      <span className="flex-shrink-0 text-xs text-zinc-500 dark:text-zinc-400">
                        {item.detail}
                      </span>
                    )}
                  </div>
                  <div className="mt-2 flex flex-wrap gap-1.5">
                    {(item.badges ?? []).map((badge) => (
                      <span
                        key={badge}
                        className="rounded-md bg-zinc-100 px-2 py-0.5 text-[11px] font-medium text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300"
                      >
                        {badge}
                      </span>
                    ))}
                  </div>
                  <div className="mt-3 flex flex-wrap gap-2">
                    {(item.actions ?? []).map((action) => (
                      <button
                        key={`${item.id}:${action.label}`}
                        type="button"
                        onClick={() => runAction(action)}
                        disabled={busyAction != null}
                        className="inline-flex items-center gap-1.5 rounded-md border border-zinc-200 px-2.5 py-1.5 text-xs font-medium text-zinc-700 transition-colors hover:bg-zinc-100 disabled:opacity-50 dark:border-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-800"
                      >
                        {action.kind === "open-url" ? (
                          <ExternalLink size={13} />
                        ) : (
                          <X size={13} />
                        )}
                        {action.label}
                      </button>
                    ))}
                  </div>
                </div>
              </article>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

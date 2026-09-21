import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { KeyRound, X } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { listInstalledPlugins } from "../../lib/tauri/plugins";

const AUTO_HIDE_MS = 15_000;

interface AttentionPayload {
  pluginId: string;
  kind: string;
}

/**
 * A plugin saying its credential was refused — an expired cookie, a
 * revoked token — so it can do nothing until the user pastes a new one.
 *
 * Until this existed the failure looked exactly like "no result": the
 * Canvas or the lyrics just stopped appearing. The backend sends
 * `plugin:attention` once per launch and per plugin
 * (`plugin_attention.rs`), however many tracks fail after it, so this
 * shows once and does not nag.
 *
 * Mounted once in AppLayout, beside the playback toast, and portalled for
 * the same reason (see the overlay invariant in CLAUDE.md).
 */
export function PluginAttentionToast() {
  const { t } = useTranslation();
  // A queue, not a slot: two plugins can be refused at once (a cookie and
  // a token expiring the same week), and each is announced only once per
  // launch — a second notice overwriting the first would lose it for good.
  const [queue, setQueue] = useState<string[]>([]);
  const notice = queue.length > 0 ? { name: queue[0] } : null;
  const dismiss = () => setQueue((prev) => prev.slice(1));

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    // Each plugin once: a notice can arrive both as an event and in the
    // catch-up read below.
    const shown = new Set<string>();
    const announce = (pluginId: string) => {
      if (shown.has(pluginId)) return;
      shown.add(pluginId);
      // The display name is the plugin's own; the id is the fallback so
      // the toast never waits on a lookup that failed.
      listInstalledPlugins()
        .then((plugins) => plugins.find((p) => p.id === pluginId)?.name)
        .catch(() => undefined)
        .then((name) => {
          if (!cancelled) setQueue((prev) => [...prev, name ?? pluginId]);
        });
    };
    listen<AttentionPayload>("plugin:attention", (event) => {
      if (event.payload.kind !== "auth-required") return;
      announce(event.payload.pluginId);
    })
      .then((off) => {
        if (cancelled) {
          off();
          return;
        }
        unlisten = off;
        // Anything announced before this listener existed: the backend
        // sends each notice once, so a missed event would never return.
        return invoke<string[]>("plugin_attention_history").then((ids) => {
          if (!cancelled) ids.forEach(announce);
        });
      })
      .catch((err) => {
        console.error("[PluginAttentionToast] listen failed", err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Keyed on the queue itself, so each notice gets its full time once it
  // reaches the head, however long it waited behind another.
  // Keyed on the head alone: a notice queued behind the one on screen must
  // not restart its countdown.
  const head = queue[0];
  useEffect(() => {
    if (head == null) return;
    const timer = window.setTimeout(
      () => setQueue((prev) => prev.slice(1)),
      AUTO_HIDE_MS,
    );
    return () => window.clearTimeout(timer);
  }, [head]);

  if (notice == null) return null;

  return createPortal(
    <div
      role="alert"
      aria-live="assertive"
      className="fixed bottom-44 left-6 z-100 w-80 rounded-2xl border border-amber-300 dark:border-amber-500/40 bg-white/95 dark:bg-zinc-900/95 backdrop-blur shadow-xl p-4 motion-safe:animate-fade-in"
    >
      <div className="flex items-start gap-3">
        <div className="shrink-0 w-9 h-9 rounded-full flex items-center justify-center bg-amber-500/15 text-amber-500">
          <KeyRound size={18} aria-hidden="true" />
        </div>
        <div className="flex-1 min-w-0">
          <div className="text-sm font-semibold text-amber-700 dark:text-amber-500">
            {t("settings.plugins.attention.authRequired.title", {
              name: notice.name,
            })}
          </div>
          <p className="mt-1 text-xs text-zinc-600 dark:text-zinc-300">
            {t("settings.plugins.attention.authRequired.body")}
          </p>
        </div>
        <button
          type="button"
          onClick={dismiss}
          aria-label={t("settings.plugins.attention.dismiss")}
          className="shrink-0 p-1 rounded-lg text-zinc-400 hover:text-zinc-600 hover:bg-zinc-100 dark:hover:text-zinc-200 dark:hover:bg-zinc-800 transition-colors"
        >
          <X size={14} />
        </button>
      </div>
    </div>,
    document.body,
  );
}

import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { HardDrive, FolderOpen, RotateCcw, AlertTriangle } from "lucide-react";
import { pickFolder } from "../../../lib/tauri/dialog";
import {
  getCacheLocation,
  setCacheLocation,
  restartForCacheMove,
  type CacheLocation,
} from "../../../lib/tauri/storage";
import { formatBytes } from "../../../lib/format";

/**
 * Settings → Data row choosing where artwork and the other rebuildable
 * caches are stored (issue #619).
 *
 * Reported by email: WaveFlow put artwork on the system drive whichever
 * drive it was installed on, and a `C:` that is critically low on space
 * takes the machine down with it.
 *
 * Two things this card must not do, both of which read as a bug rather
 * than a feature:
 *
 * - **Pretend the move took effect.** The backend resolves its paths
 *   once at boot, so a move needs a restart. The card says so and offers
 *   the restart instead of leaving the user to guess why their next
 *   cover still landed on the old drive.
 * - **Stay quiet about a fallback.** A configured drive that is
 *   unplugged at launch drops back to the default location. Caches
 *   reappearing where they used to be looks exactly like them having
 *   been wiped, so the reason is shown verbatim.
 */
export function CacheLocationCard({ language }: { language: string }) {
  const { t } = useTranslation();
  const [location, setLocation] = useState<CacheLocation | null>(null);
  const [busy, setBusy] = useState(false);
  /** The native folder dialog is up. See `onChoose`. */
  const [picking, setPicking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The initial read, inside an async IIFE rather than behind a
  // `refresh()` helper: the lint rule reads a synchronous call in an
  // effect body as a cascading render even when the setState is behind
  // an await, and the guard flag this shape needs anyway is what stops a
  // late answer from painting into an unmounted card.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const next = await getCacheLocation();
        if (!cancelled) setLocation(next);
      } catch (err) {
        console.warn("[CacheLocationCard] read failed", err);
        // Kept in state, not only in the console: `if (!location)
        // return null` below would otherwise make the whole row vanish
        // from Settings, which reads as the feature not existing rather
        // than as a read having failed.
        if (!cancelled)
          setError(err instanceof Error ? err.message : String(err));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const move = useCallback(async (root: string | null) => {
    setBusy(true);
    setError(null);
    try {
      setLocation(await setCacheLocation(root));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, []);

  const onChoose = useCallback(async () => {
    // Separate from `busy`, which means "a copy is running" and drives
    // the message under the buttons. This one only has to keep a second
    // native dialog from opening behind the first: the button stays
    // clickable for as long as the picker is up, and two moves staged
    // from one card is the exact race the backend mutex had to be added
    // for.
    setPicking(true);
    try {
      const picked = await pickFolder(t("settings.cacheLocation.pickTitle"));
      if (picked) await move(picked);
    } catch (err) {
      // The native picker can fail outright — no portal on a headless
      // Linux session, a denied permission on macOS. Silently doing
      // nothing reads as a dead button.
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setPicking(false);
    }
  }, [move, t]);

  if (!location) {
    // No retry button here, and that is deliberate. The read is a
    // SELECT on `app.db`; when it fails the card renders the reason and
    // nothing else, and re-entering Settings re-runs it -- so the
    // recovery exists, it just is not a button. Adding one costs a new
    // string in seventeen locales for a path the user reaches by
    // navigating away and back.
    if (!error) return null;
    return (
      <section
        aria-label={t("settings.cacheLocation.title")}
        className="py-5 px-4 rounded-xl"
      >
        <div className="flex items-start gap-3">
          <HardDrive
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <div className="min-w-0">
            <div className="text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.cacheLocation.title")}
            </div>
            <p
              className="text-xs text-red-600 dark:text-red-400 mt-1"
              role="alert"
            >
              {error}
            </p>
          </div>
        </div>
      </section>
    );
  }

  const moved = location.configured_root !== null;

  return (
    <section
      aria-label={t("settings.cacheLocation.title")}
      className="py-5 px-4 rounded-xl"
    >
      <div className="flex items-start justify-between gap-4">
        <div className="flex items-start space-x-4 min-w-0">
          <HardDrive
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <div className="min-w-0">
            <div className="text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.cacheLocation.title")}
            </div>
            <div className="text-xs mt-0.5 settings-description">
              {t("settings.cacheLocation.subtitle")}
            </div>
            <div
              className="text-xs text-zinc-600 dark:text-zinc-300 mt-2 break-all font-mono"
              title={location.active_root}
            >
              {location.active_root}
            </div>
            <div className="text-xs text-zinc-500 dark:text-zinc-400 mt-1">
              {t("settings.cacheLocation.size", {
                size: formatBytes(location.size_bytes, language),
              })}
            </div>
          </div>
        </div>
        <div className="flex flex-col items-end gap-2 shrink-0">
          <button
            type="button"
            onClick={() => void onChoose()}
            disabled={busy || picking}
            className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 disabled:opacity-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
          >
            <FolderOpen size={14} aria-hidden="true" />
            <span>{t("settings.cacheLocation.choose")}</span>
          </button>
          {moved && (
            <button
              type="button"
              onClick={() => void move(null)}
              disabled={busy || picking}
              className="flex items-center space-x-2 px-4 py-2 rounded-xl text-sm font-medium text-zinc-500 hover:text-zinc-800 disabled:opacity-50 dark:text-zinc-400 dark:hover:text-zinc-100 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
            >
              <RotateCcw size={14} aria-hidden="true" />
              <span>{t("settings.cacheLocation.reset")}</span>
            </button>
          )}
        </div>
      </div>

      {busy && (
        <p
          className="text-xs text-zinc-500 dark:text-zinc-400 mt-3"
          role="status"
        >
          {t("settings.cacheLocation.copying")}
        </p>
      )}

      {location.fell_back && (
        <p
          className="flex items-start gap-2 text-xs text-amber-600 dark:text-amber-400 mt-3"
          role="status"
        >
          <AlertTriangle
            size={14}
            className="mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <span>
            {t("settings.cacheLocation.fellBack", {
              path: location.configured_root ?? "",
            })}
            {location.fallback_reason ? ` (${location.fallback_reason})` : ""}
          </span>
        </p>
      )}

      {location.restart_required && (
        <div className="flex items-center justify-between gap-3 mt-3 p-3 rounded-xl bg-emerald-50 dark:bg-emerald-950/40 border border-emerald-500/30">
          <p className="text-xs text-emerald-700 dark:text-emerald-300">
            {t("settings.cacheLocation.restartNeeded")}
          </p>
          <button
            type="button"
            // The copy has to be durable before the process may be
            // replaced. The backend waits on the same lock, so this is
            // the honest surface rather than the guarantee.
            disabled={busy || picking}
            onClick={() => {
              // The command replaces the process and normally never
              // resolves; a rejection means it could not, and the user
              // is left looking at a button that did nothing.
              restartForCacheMove().catch((err) => {
                setError(err instanceof Error ? err.message : String(err));
              });
            }}
            className="shrink-0 px-3 py-1.5 rounded-lg bg-emerald-500 text-white text-xs font-medium hover:bg-emerald-600 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
          >
            {t("settings.cacheLocation.restartNow")}
          </button>
        </div>
      )}

      {error && (
        <p className="text-xs text-red-600 dark:text-red-400 mt-3" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

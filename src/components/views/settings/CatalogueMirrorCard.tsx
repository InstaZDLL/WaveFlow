import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { Library, Loader2, RefreshCw, Trash2, X } from "lucide-react";
import {
  remoteCancelCatalogueMirror,
  remoteCatalogueStats,
  remoteClearCatalogue,
  remoteGetStatus,
  remoteArtworkCacheInfo,
  remoteClearArtworkCache,
  remoteMirrorCatalogue,
  remoteClearDownloads,
  remoteClearStreamCache,
  remoteDownloadsInfo,
  remoteStreamCacheInfo,
  type DownloadsInfo,
  type ArtworkCacheInfo,
  type StreamCacheInfo,
  type CatalogueMirrorProgress,
  type CatalogueMirrorReport,
  type CatalogueStats,
} from "../../../lib/tauri/remoteServer";

/** Mebibytes with one decimal, in the interface's language. Covers are tens
 *  of kilobytes each, so the interesting figure is always the total. */
function formatBytes(bytes: number, locale: string): string {
  return new Intl.NumberFormat(locale, {
    maximumFractionDigits: 1,
  }).format(bytes / (1024 * 1024));
}

/**
 * Settings → the server's catalogue, mirrored locally.
 *
 * The projection only ever holds the tracks the account touched — a playlist's
 * songs, the queue, the favourites. Everything else exists solely on the
 * server, which is why the remote source can show playlists and nothing else.
 * Walking the catalogue in is what lets both sources appear in one library.
 *
 * Kept beside the connection card rather than inside it: that card binds a
 * profile to a server and does nothing else, on purpose.
 *
 * Hides itself when `sync_v2` is absent, like every other remote surface —
 * TypeScript cannot see a Cargo feature, so the card probes for its backend.
 */
export function CatalogueMirrorCard() {
  const { t, i18n } = useTranslation();
  const [visible, setVisible] = useState(false);
  const [stats, setStats] = useState<CatalogueStats | null>(null);
  const [report, setReport] = useState<CatalogueMirrorReport | null>(null);
  const [progress, setProgress] = useState<CatalogueMirrorProgress | null>(null);
  const [covers, setCovers] = useState<ArtworkCacheInfo | null>(null);
  const [streams, setStreams] = useState<StreamCacheInfo | null>(null);
  const [downloads, setDownloads] = useState<DownloadsInfo | null>(null);
  const [busy, setBusy] = useState<
    null | "mirror" | "clear" | "covers" | "streams" | "downloads"
  >(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mountedRef = useRef(false);

  const refreshStats = useCallback(async () => {
    const next = await remoteCatalogueStats();
    if (mountedRef.current) setStats(next);
    try {
      const cache = await remoteArtworkCacheInfo();
      if (mountedRef.current) setCovers(cache);
    } catch {
      // Informational only — keep the last figure rather than blanking it.
    }
    try {
      const cache = await remoteStreamCacheInfo();
      if (mountedRef.current) setStreams(cache);
    } catch {
      // Same: a figure that failed to refresh is better than none.
    }
    try {
      const kept = await remoteDownloadsInfo();
      if (mountedRef.current) setDownloads(kept);
    } catch {
      // Same again.
    }
  }, []);

  // Subscribe before the first read: the walk can start from another window,
  // and an event emitted while `listen()` is still resolving is lost for good.
  useEffect(() => {
    mountedRef.current = true;
    let unlisten: (() => void) | undefined;
    void (async () => {
      try {
        unlisten = await listen<CatalogueMirrorProgress>(
          "remote:mirror-progress",
          (event) => {
            if (mountedRef.current) setProgress(event.payload);
          },
        );
      } catch {
        // Progress is decoration; the card works without it.
      }
      try {
        const status = await remoteGetStatus();
        if (!mountedRef.current) return;
        const nextVisible = status.signed_in && status.bootstrapped;
        setVisible(nextVisible);
        if (nextVisible) await refreshStats();
      } catch {
        if (mountedRef.current) setVisible(false);
      }
    })();
    return () => {
      mountedRef.current = false;
      if (unlisten) unlisten();
    };
  }, [refreshStats]);

  const mirror = useCallback(async () => {
    setBusy("mirror");
    setError(null);
    setReport(null);
    setProgress(null);
    try {
      const next = await remoteMirrorCatalogue();
      // Another walk owns the slot (a double click): keep what is on screen
      // rather than replacing it with an all-zero report.
      if (next.already_running) return;
      if (mountedRef.current) setReport(next);
      await refreshStats();
    } catch (err) {
      if (mountedRef.current) setError(String(err));
    } finally {
      if (mountedRef.current) {
        setBusy(null);
        setProgress(null);
      }
    }
  }, [refreshStats]);

  const cancel = useCallback(() => {
    void remoteCancelCatalogueMirror().catch(() => {});
  }, []);

  const clearCovers = useCallback(async () => {
    setBusy("covers");
    setError(null);
    try {
      await remoteClearArtworkCache();
      await refreshStats();
    } catch (err) {
      if (mountedRef.current) setError(String(err));
    } finally {
      if (mountedRef.current) setBusy(null);
    }
  }, [refreshStats]);

  const clearStreams = useCallback(async () => {
    setBusy("streams");
    setError(null);
    try {
      await remoteClearStreamCache();
      await refreshStats();
    } catch (err) {
      if (mountedRef.current) setError(String(err));
    } finally {
      if (mountedRef.current) setBusy(null);
    }
  }, [refreshStats]);

  const clearDownloads = useCallback(async () => {
    setBusy("downloads");
    setError(null);
    try {
      await remoteClearDownloads();
      await refreshStats();
    } catch (err) {
      if (mountedRef.current) setError(String(err));
    } finally {
      if (mountedRef.current) setBusy(null);
    }
  }, [refreshStats]);

  const clear = useCallback(async () => {
    setBusy("clear");
    setError(null);
    try {
      await remoteClearCatalogue();
      if (mountedRef.current) setReport(null);
      await refreshStats();
    } catch (err) {
      if (mountedRef.current) setError(String(err));
    } finally {
      if (mountedRef.current) {
        setBusy(null);
        setConfirmClear(false);
      }
    }
  }, [refreshStats]);

  if (!visible) return null;

  const running = busy === "mirror";
  // Albums are not the only thing the mirror holds: a server whose singles
  // belong to no album mirrors tracks and nothing else, and gating on albums
  // alone would leave that user unable to clear anything.
  const hasSomethingToClear =
    (stats?.albums ?? 0) > 0 ||
    (stats?.tracks ?? 0) > 0 ||
    (stats?.libraries ?? 0) > 0;

  return (
    <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
      <div className="flex items-start space-x-4">
        <Library size={20} className="text-zinc-400 mt-0.5" aria-hidden="true" />
        <div className="flex-1 min-w-0 space-y-4">
          <div className="flex items-start justify-between gap-4">
            <div>
              <h3 className="text-sm font-medium text-zinc-900 dark:text-zinc-100">
                {t("remote.catalogue.title")}
              </h3>
              <p className="text-xs text-zinc-500 dark:text-zinc-400">
                {t("remote.catalogue.subtitle")}
              </p>
            </div>
            <div className="shrink-0 flex items-center gap-2">
              {running && (
                <button
                  type="button"
                  onClick={cancel}
                  className="px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 inline-flex items-center gap-1.5"
                >
                  <X size={14} aria-hidden="true" />
                  {t("remote.catalogue.cancel")}
                </button>
              )}
              <button
                type="button"
                onClick={() => void mirror()}
                disabled={busy !== null}
                className="px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 inline-flex items-center gap-1.5 disabled:opacity-50"
              >
                {running ? (
                  <Loader2 size={14} className="animate-spin" aria-hidden="true" />
                ) : (
                  <RefreshCw size={14} aria-hidden="true" />
                )}
                {t("remote.catalogue.mirror")}
              </button>
            </div>
          </div>

          {stats && (
            <dl className="grid grid-cols-2 sm:grid-cols-4 gap-3 text-sm">
              <div>
                <dt className="text-xs text-zinc-500 dark:text-zinc-400">
                  {t("remote.catalogue.statAlbums")}
                </dt>
                <dd className="tabular-nums text-zinc-900 dark:text-zinc-100">
                  {stats.albums_mirrored === stats.albums
                    ? stats.albums
                    : `${stats.albums_mirrored} / ${stats.albums}`}
                </dd>
              </div>
              <div>
                <dt className="text-xs text-zinc-500 dark:text-zinc-400">
                  {t("remote.catalogue.statTracks")}
                </dt>
                <dd className="tabular-nums text-zinc-900 dark:text-zinc-100">
                  {stats.tracks}
                </dd>
              </div>
              <div>
                <dt className="text-xs text-zinc-500 dark:text-zinc-400">
                  {t("remote.catalogue.statArtists")}
                </dt>
                <dd className="tabular-nums text-zinc-900 dark:text-zinc-100">
                  {stats.artists}
                </dd>
              </div>
              <div>
                <dt className="text-xs text-zinc-500 dark:text-zinc-400">
                  {t("remote.catalogue.statLibraries")}
                </dt>
                <dd className="tabular-nums text-zinc-900 dark:text-zinc-100">
                  {stats.libraries}
                </dd>
              </div>
            </dl>
          )}

          {/* The walk cannot know its own total until the last page comes back,
              so this is a running count, never a percentage. */}
          {progress && (
            <p className="text-xs text-zinc-500 dark:text-zinc-400">
              {progress.phase === "albums"
                ? t("remote.catalogue.progressAlbums", { done: progress.done })
                : t("remote.catalogue.progressSweep", { done: progress.done })}
            </p>
          )}

          {report && !running && (
            <p className="text-xs text-zinc-600 dark:text-zinc-300">
              {report.albums_walked === 0 && report.orphans_mirrored === 0
                ? t("remote.catalogue.reportUnchanged")
                : t("remote.catalogue.reportWalked", {
                    albums: report.albums_walked,
                    tracks: report.tracks_mirrored + report.orphans_mirrored,
                  })}
              {report.removed > 0 &&
                ` · ${t("remote.catalogue.reportRemoved", { removed: report.removed })}`}
              {report.cancelled && ` · ${t("remote.catalogue.reportCancelled")}`}
            </p>
          )}

          {/* The covers live on disk, not in the database, so they are counted
              apart from the mirror and cleared apart from it — dropping them
              costs one download each, never a wrong picture. */}
          {covers && covers.covers > 0 && (
            <p className="text-xs text-zinc-500 dark:text-zinc-400 flex items-center gap-2 flex-wrap">
              <span>
                {/* `count` drives i18next's plural selection; the locales
                    carry exactly the CLDR categories each of them declares
                    elsewhere, so no language falls through to English. */}
                {t("remote.catalogue.coversCached", {
                  count: covers.covers,
                  size: formatBytes(
                    covers.bytes,
                    i18n.resolvedLanguage ?? i18n.language,
                  ),
                })}
              </span>
              <button
                type="button"
                onClick={() => void clearCovers()}
                disabled={busy !== null}
                className="underline underline-offset-2 disabled:opacity-50"
              >
                {t("remote.catalogue.clearCovers")}
              </button>
            </p>
          )}

          {/* Cached audio, counted and cleared apart from the covers and from
              the mirror. Dropping it costs one download per track played
              again — the server still holds every byte. */}
          {streams && streams.tracks > 0 && (
            <p className="text-xs text-zinc-500 dark:text-zinc-400 flex items-center gap-2 flex-wrap">
              <span>
                {t("remote.catalogue.streamsCached", {
                  count: streams.tracks,
                  size: formatBytes(
                    streams.bytes,
                    i18n.resolvedLanguage ?? i18n.language,
                  ),
                })}
              </span>
              <button
                type="button"
                onClick={() => void clearStreams()}
                disabled={busy !== null}
                className="underline underline-offset-2 disabled:opacity-50"
              >
                {t("remote.catalogue.clearStreams")}
              </button>
            </p>
          )}

          {/* Offline copies, counted apart from the cache above and cleared
              apart from it: the cache is evicted under a budget without
              asking, and these disappear only when their owner says so. */}
          {downloads && downloads.tracks > 0 && (
            <p className="text-xs text-zinc-500 dark:text-zinc-400 flex items-center gap-2 flex-wrap">
              <span>
                {t("remote.catalogue.downloadsKept", {
                  count: downloads.tracks,
                  size: formatBytes(
                    downloads.bytes,
                    i18n.resolvedLanguage ?? i18n.language,
                  ),
                })}
              </span>
              <button
                type="button"
                onClick={() => void clearDownloads()}
                disabled={busy !== null}
                className="underline underline-offset-2 disabled:opacity-50"
              >
                {t("remote.catalogue.clearDownloads")}
              </button>
            </p>
          )}

          {stats && (
            <p className="text-xs text-zinc-500 dark:text-zinc-400">
              {stats.mirrored_at === null
                ? t("remote.catalogue.neverMirrored")
                : t("remote.catalogue.mirroredAt", {
                    // The interface language, not the machine's: a French UI on
                    // an English system must not date itself in English.
                    date: new Date(stats.mirrored_at).toLocaleString(
                      i18n.resolvedLanguage ?? i18n.language,
                    ),
                  })}
            </p>
          )}

          {error && (
            <p className="text-xs text-red-600 dark:text-red-400">{error}</p>
          )}

          <div className="pt-1">
            {confirmClear ? (
              <div className="flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => void clear()}
                  disabled={busy !== null}
                  className="px-3 py-1.5 text-sm rounded-lg border border-red-300 dark:border-red-800 text-red-600 dark:text-red-400 inline-flex items-center gap-1.5 disabled:opacity-50"
                >
                  {busy === "clear" ? (
                    <Loader2 size={14} className="animate-spin" aria-hidden="true" />
                  ) : (
                    <Trash2 size={14} aria-hidden="true" />
                  )}
                  {t("remote.catalogue.clearConfirm")}
                </button>
                <button
                  type="button"
                  onClick={() => setConfirmClear(false)}
                  className="px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700"
                >
                  {t("remote.catalogue.clearAbort")}
                </button>
              </div>
            ) : (
              <button
                type="button"
                onClick={() => setConfirmClear(true)}
                disabled={busy !== null || !hasSomethingToClear}
                className="px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 inline-flex items-center gap-1.5 disabled:opacity-50"
              >
                <Trash2 size={14} aria-hidden="true" />
                {t("remote.catalogue.clear")}
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

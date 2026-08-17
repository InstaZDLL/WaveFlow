import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  ArrowLeftRight,
  Check,
  Link2,
  Loader2,
  RefreshCw,
  Unlink,
  X,
} from "lucide-react";
import { listPlaylists, type Playlist } from "../../../lib/tauri/playlist";
import {
  remoteCancelReconcileScan,
  remoteConvertPlaylist,
  remoteConfirmReconciliation,
  remoteGetStatus,
  remoteCopyReconciliationFavorite,
  remoteCopyReconciliationRating,
  remoteListReconciliationLinks,
  remoteListPlaylists,
  remotePreviewPlaylistConversion,
  remoteReconcileScan,
  remoteRejectReconciliation,
  remoteRemoveReconciliationLink,
  remoteSetReconciliationPreference,
  type MatchCandidateGroup,
  type PlaylistConversionDirection,
  type PlaylistConversionPreview,
  type ReconcileProgress,
  type ReconciliationLink,
  type ReconciliationReport,
  type RemotePlaylistSummary,
} from "../../../lib/tauri/remoteServer";

/** M5 identity links. Kept beside the connection card because it only makes
 * sense for a bootstrapped native remote source. */
export function ReconciliationCard() {
  const [visible, setVisible] = useState(false);
  const [links, setLinks] = useState<ReconciliationLink[]>([]);
  const [report, setReport] = useState<ReconciliationReport | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [progress, setProgress] = useState<ReconcileProgress | null>(null);

  const refreshLinks = useCallback(async () => {
    setLinks(await remoteListReconciliationLinks());
  }, []);

  // The scan hashes files on a background thread and emits
  // `reconcile:progress` per file, so the bar advances even on a large scan.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    // If the effect is torn down before `listen()` resolves, the cleanup below
    // sees `unlisten` still null and can't detach; mark disposal so the late
    // resolution unlistens immediately instead of leaking the subscription.
    let disposed = false;
    listen<ReconcileProgress>("reconcile:progress", (event) => {
      setProgress(event.payload);
    })
      .then((un) => {
        if (disposed) un();
        else unlisten = un;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const status = await remoteGetStatus();
        if (cancelled) return;
        const nextVisible = status.signed_in && status.bootstrapped;
        setVisible(nextVisible);
        if (nextVisible) await refreshLinks();
      } catch {
        if (!cancelled) setVisible(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [refreshLinks]);

  const scan = useCallback(async () => {
    setBusy("scan");
    setError(null);
    setProgress(null);
    try {
      const next = await remoteReconcileScan();
      setReport(next);
      await refreshLinks();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(null);
      setProgress(null);
    }
  }, [refreshLinks]);

  const cancelScan = useCallback(() => {
    void remoteCancelReconcileScan().catch(() => {});
  }, []);

  const runPair = useCallback(
    async (key: string, action: () => Promise<void>, rescan: boolean) => {
      setBusy(key);
      setError(null);
      try {
        await action();
        if (rescan) setReport(await remoteReconcileScan());
        await refreshLinks();
      } catch (err) {
        setError(String(err));
      } finally {
        setBusy(null);
      }
    },
    [refreshLinks],
  );

  if (!visible) return null;

  return (
    <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
      <div className="flex items-start space-x-4">
        <Link2 size={20} className="text-zinc-400 mt-0.5" aria-hidden="true" />
        <div className="flex-1 min-w-0 space-y-4">
          <div className="flex items-start justify-between gap-4">
            <div>
              <h3 className="text-sm font-medium text-zinc-900 dark:text-zinc-100">
                Local ↔ server matching
              </h3>
              <p className="text-xs text-zinc-500 dark:text-zinc-400">
                Exact full-file matches only. Same-size files are verified with
                BLAKE3; metadata never creates a link.
              </p>
            </div>
            <div className="shrink-0 flex items-center gap-2">
              {busy === "scan" && (
                <button
                  type="button"
                  onClick={cancelScan}
                  className="px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 inline-flex items-center gap-1.5"
                >
                  <X size={14} aria-hidden="true" />
                  Cancel
                </button>
              )}
              <button
                type="button"
                onClick={() => void scan()}
                disabled={busy !== null}
                className="px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 inline-flex items-center gap-1.5 disabled:opacity-50"
              >
                {busy === "scan" ? (
                  <Loader2
                    size={14}
                    className="animate-spin"
                    aria-hidden="true"
                  />
                ) : (
                  <RefreshCw size={14} aria-hidden="true" />
                )}
                Find matches
              </button>
            </div>
          </div>

          {busy === "scan" && progress && progress.total > 0 && (
            <p
              className="text-xs text-zinc-500 dark:text-zinc-400"
              role="status"
              aria-live="polite"
            >
              Hashing {progress.processed} / {progress.total} local files…
            </p>
          )}

          {report && <ReportSummary report={report} />}

          {report?.candidates.map((group) => (
            <CandidateEditor
              key={group.full_hash}
              group={group}
              busy={busy}
              onConfirm={(localId, remoteId) =>
                runPair(
                  `confirm:${localId}:${remoteId}`,
                  () => remoteConfirmReconciliation(localId, remoteId),
                  true,
                )
              }
              onReject={(localId, remoteId) =>
                runPair(
                  `reject:${localId}:${remoteId}`,
                  () => remoteRejectReconciliation(localId, remoteId),
                  true,
                )
              }
            />
          ))}

          {links.length > 0 && (
            <div className="space-y-2">
              <p className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase">
                Confirmed links
              </p>
              {links.map((link) => (
                <div
                  key={link.local_track_id}
                  className="flex items-start gap-3 rounded-lg border border-zinc-200 dark:border-zinc-700 px-3 py-2"
                >
                  <div className="min-w-0 flex-1">
                    <p className="text-sm text-zinc-800 dark:text-zinc-100 truncate">
                      {link.local_title}
                    </p>
                    <p className="text-xs text-zinc-500 truncate">
                      {link.remote_title ?? link.remote_track_id}
                    </p>
                    <p className="text-[10px] text-zinc-500 mt-1">
                      Favorites: local {link.local_favorite ? "yes" : "no"} ·
                      server {link.remote_favorite ? "yes" : "no"} · Ratings:
                      local {formatLocalRating(link.local_rating)} · server{" "}
                      {link.remote_rating ?? "—"}/5
                    </p>
                    <p className="text-[10px] text-zinc-500">
                      History: {link.local_plays} local + {link.remote_plays}{" "}
                      server = {link.combined_plays} plays
                    </p>
                    <div className="flex flex-wrap gap-1 mt-1.5">
                      {[
                        [
                          "favorite",
                          "local_to_server",
                          "Favorite local → server",
                        ],
                        [
                          "favorite",
                          "server_to_local",
                          "Favorite server → local",
                        ],
                        ["rating", "local_to_server", "Rating local → server"],
                        ["rating", "server_to_local", "Rating server → local"],
                      ].map(([kind, direction, label]) => (
                        <button
                          key={`${kind}:${direction}`}
                          type="button"
                          disabled={
                            busy !== null || link.status !== "confirmed"
                          }
                          onClick={() =>
                            void runPair(
                              `copy:${kind}:${direction}:${link.local_track_id}`,
                              () =>
                                kind === "favorite"
                                  ? remoteCopyReconciliationFavorite(
                                      link.local_track_id,
                                      direction as PlaylistConversionDirection,
                                    )
                                  : remoteCopyReconciliationRating(
                                      link.local_track_id,
                                      direction as PlaylistConversionDirection,
                                    ),
                              false,
                            )
                          }
                          className="px-2 py-1 text-[10px] rounded border border-zinc-200 dark:border-zinc-700 disabled:opacity-40"
                        >
                          {label}
                        </button>
                      ))}
                    </div>
                  </div>
                  <span
                    className={`text-[10px] font-medium uppercase ${
                      link.status === "stale"
                        ? "text-amber-600 dark:text-amber-400"
                        : "text-emerald-600 dark:text-emerald-400"
                    }`}
                  >
                    {link.status}
                  </span>
                  <select
                    value={link.playback_preference}
                    disabled={busy !== null}
                    onChange={(event) => {
                      const preference = event.target.value as
                        "local_first" | "server_first";
                      void runPair(
                        `preference:${link.local_track_id}`,
                        () =>
                          remoteSetReconciliationPreference(
                            link.local_track_id,
                            preference,
                          ),
                        false,
                      );
                    }}
                    aria-label={`Playback preference for ${link.local_title}`}
                    className="px-2 py-1 text-xs rounded-md border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900"
                  >
                    <option value="local_first">Local first</option>
                    <option value="server_first">Server first</option>
                  </select>
                  <button
                    type="button"
                    onClick={() =>
                      void runPair(
                        `unlink:${link.local_track_id}`,
                        () =>
                          remoteRemoveReconciliationLink(link.local_track_id),
                        false,
                      )
                    }
                    disabled={busy !== null}
                    aria-label={`Unlink ${link.local_title}`}
                    className="p-1.5 rounded-md text-zinc-500 hover:text-red-600 hover:bg-red-50 dark:hover:bg-red-950/30 disabled:opacity-50"
                  >
                    <Unlink size={14} aria-hidden="true" />
                  </button>
                </div>
              ))}
            </div>
          )}

          <PlaylistConversion />

          {error && (
            <p
              role="status"
              aria-live="polite"
              className="text-xs text-red-600 dark:text-red-400 break-words"
            >
              {error}
            </p>
          )}
        </div>
      </div>
    </div>
  );
}

function formatLocalRating(value: number | null): string {
  return value == null ? "—/5" : `${((value / 255) * 5).toFixed(1)}/5`;
}

function PlaylistConversion() {
  const [direction, setDirection] =
    useState<PlaylistConversionDirection>("local_to_server");
  const [localPlaylists, setLocalPlaylists] = useState<Playlist[]>([]);
  const [remotePlaylists, setRemotePlaylists] = useState<
    RemotePlaylistSummary[]
  >([]);
  const [sourceId, setSourceId] = useState("");
  const [preview, setPreview] = useState<PlaylistConversionPreview | null>(
    null,
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  const loadPlaylists = useCallback(async (isCancelled?: () => boolean) => {
    const [local, remote] = await Promise.all([
      listPlaylists(),
      remoteListPlaylists(),
    ]);
    if (isCancelled?.()) return;
    setLocalPlaylists(local.filter((playlist) => playlist.is_smart === 0));
    setRemotePlaylists(remote);
  }, []);

  useEffect(() => {
    let cancelled = false;
    void Promise.resolve()
      .then(() => loadPlaylists(() => cancelled))
      .catch((err) => {
        if (!cancelled) setError(String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [loadPlaylists]);

  const sources =
    direction === "local_to_server" ? localPlaylists : remotePlaylists;
  const selectedSourceId = sources.some(
    (playlist) => String(playlist.id) === sourceId,
  )
    ? sourceId
    : sources[0]
      ? String(sources[0].id)
      : "";

  const inspect = useCallback(async () => {
    if (!selectedSourceId) return;
    setBusy(true);
    setError(null);
    setSuccess(null);
    try {
      setPreview(
        await remotePreviewPlaylistConversion(direction, selectedSourceId),
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }, [direction, selectedSourceId]);

  const convert = useCallback(async () => {
    if (!selectedSourceId || !preview?.can_convert) return;
    setBusy(true);
    setError(null);
    setSuccess(null);
    try {
      const result = await remoteConvertPlaylist(direction, selectedSourceId);
      setSuccess(
        `${result.converted_tracks} tracks copied to ${
          direction === "local_to_server" ? "the server" : "the local library"
        }.`,
      );
      setPreview(null);
      await loadPlaylists();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }, [direction, loadPlaylists, preview?.can_convert, selectedSourceId]);

  return (
    <div className="border-t border-zinc-200 dark:border-zinc-800 pt-4 space-y-3">
      <div className="flex items-center gap-2">
        <ArrowLeftRight
          size={15}
          className="text-zinc-400"
          aria-hidden="true"
        />
        <div>
          <p className="text-xs font-medium text-zinc-800 dark:text-zinc-100">
            Explicit playlist copy
          </p>
          <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
            A preview must confirm every identity link. Nothing is merged or
            omitted silently.
          </p>
        </div>
      </div>

      <div className="grid grid-cols-1 xl:grid-cols-[180px_minmax(0,1fr)_auto] gap-2">
        <select
          value={direction}
          onChange={(event) => {
            setDirection(event.target.value as PlaylistConversionDirection);
            setSourceId("");
            setPreview(null);
            setSuccess(null);
          }}
          disabled={busy}
          aria-label="Playlist copy direction"
          className="px-2 py-1.5 text-xs rounded-md border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900"
        >
          <option value="local_to_server">Local → server</option>
          <option value="server_to_local">Server → local</option>
        </select>
        <select
          value={selectedSourceId}
          onChange={(event) => {
            setSourceId(event.target.value);
            setPreview(null);
            setSuccess(null);
          }}
          disabled={busy || sources.length === 0}
          aria-label="Source playlist"
          className="min-w-0 px-2 py-1.5 text-xs rounded-md border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900"
        >
          {sources.length === 0 && <option value="">No playlists</option>}
          {sources.map((playlist) => (
            <option key={playlist.id} value={String(playlist.id)}>
              {playlist.name} · {playlist.track_count} tracks
            </option>
          ))}
        </select>
        <button
          type="button"
          onClick={() => void inspect()}
          disabled={busy || !selectedSourceId}
          className="px-3 py-1.5 text-xs rounded-md border border-zinc-200 dark:border-zinc-700 disabled:opacity-50"
        >
          Preview
        </button>
      </div>

      {preview && (
        <div className="rounded-lg border border-zinc-200 dark:border-zinc-700 p-3 space-y-3">
          <p className="text-xs text-zinc-600 dark:text-zinc-300">
            {preview.source_name}: {preview.convertible_tracks}/
            {preview.total_tracks} tracks have confirmed links
            {preview.blocked_tracks > 0
              ? ` · ${preview.blocked_tracks} blocked`
              : ""}
          </p>
          {preview.blocked_tracks > 0 && (
            <div className="max-h-40 overflow-auto space-y-1">
              {preview.items
                .filter((item) => item.status !== "confirmed")
                .map((item) => (
                  <div
                    key={`${item.position}:${item.remote_track_id ?? item.local_track_id}`}
                    className="flex items-center justify-between gap-3 text-[11px]"
                  >
                    <span className="truncate">{item.title}</span>
                    <span className="shrink-0 text-amber-600 dark:text-amber-400">
                      {item.status.replace(/_/g, " ")}
                    </span>
                  </div>
                ))}
            </div>
          )}
          <button
            type="button"
            onClick={() => void convert()}
            disabled={busy || !preview.can_convert}
            className="px-3 py-1.5 text-xs rounded-md bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900 disabled:opacity-40"
          >
            Confirm copy
          </button>
        </div>
      )}

      {success && (
        <p
          role="status"
          aria-live="polite"
          className="text-xs text-emerald-600 dark:text-emerald-400"
        >
          {success}
        </p>
      )}
      {error && (
        <p
          role="status"
          aria-live="polite"
          className="text-xs text-red-600 dark:text-red-400"
        >
          {error}
        </p>
      )}
    </div>
  );
}

function ReportSummary({ report }: { report: ReconciliationReport }) {
  if (report.cancelled) {
    return (
      <p className="text-xs text-zinc-500 dark:text-zinc-400">
        Scan cancelled — nothing was linked.
      </p>
    );
  }
  return (
    <p className="text-xs text-zinc-500 dark:text-zinc-400">
      {report.hashed_local_tracks} local candidates verified ·{" "}
      {report.auto_linked} linked automatically · {report.candidates.length}{" "}
      groups need a decision
      {report.stale_links > 0 ? ` · ${report.stale_links} stale` : ""}
      {report.unreadable_local_tracks > 0
        ? ` · ${report.unreadable_local_tracks} unreadable`
        : ""}
    </p>
  );
}

function CandidateEditor({
  group,
  busy,
  onConfirm,
  onReject,
}: {
  group: MatchCandidateGroup;
  busy: string | null;
  onConfirm: (localId: number, remoteId: string) => Promise<void>;
  onReject: (localId: number, remoteId: string) => Promise<void>;
}) {
  const [localId, setLocalId] = useState(group.local_tracks[0]?.track_id ?? 0);
  const [remoteId, setRemoteId] = useState(
    group.remote_tracks[0]?.track_id ?? "",
  );

  // A rescan hands a fresh `group` under the same `full_hash` key, so this
  // instance (and its selection state) is reused — a stored id can now point
  // at a track the rescan removed. Derive the effective ids from the current
  // group, falling back to the first entry, so the selects and the
  // confirm/reject handlers only ever act on ids still present.
  const effectiveLocalId = group.local_tracks.some(
    (t) => t.track_id === localId,
  )
    ? localId
    : (group.local_tracks[0]?.track_id ?? 0);
  const effectiveRemoteId = group.remote_tracks.some(
    (t) => t.track_id === remoteId,
  )
    ? remoteId
    : (group.remote_tracks[0]?.track_id ?? "");

  return (
    <div className="rounded-lg border border-amber-200 dark:border-amber-900/60 bg-amber-50/40 dark:bg-amber-950/10 p-3 space-y-2">
      <p className="text-xs font-medium text-amber-800 dark:text-amber-300">
        Identical copies need confirmation
      </p>
      <div className="grid grid-cols-1 xl:grid-cols-2 gap-2">
        <select
          value={effectiveLocalId}
          onChange={(event) => setLocalId(Number(event.target.value))}
          disabled={busy !== null}
          aria-label="Local track"
          className="min-w-0 px-2 py-1.5 text-xs rounded-md border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900"
        >
          {group.local_tracks.map((track) => (
            <option key={track.track_id} value={track.track_id}>
              Local: {track.title} — {track.file_path}
            </option>
          ))}
        </select>
        <select
          value={effectiveRemoteId}
          onChange={(event) => setRemoteId(event.target.value)}
          disabled={busy !== null}
          aria-label="Server track"
          className="min-w-0 px-2 py-1.5 text-xs rounded-md border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900"
        >
          {group.remote_tracks.map((track) => (
            <option key={track.track_id} value={track.track_id}>
              Server: {track.title}
              {track.artist ? ` — ${track.artist}` : ""}
            </option>
          ))}
        </select>
      </div>
      <div className="flex gap-2">
        <button
          type="button"
          onClick={() => void onConfirm(effectiveLocalId, effectiveRemoteId)}
          disabled={busy !== null || !effectiveRemoteId}
          className="px-2.5 py-1 text-xs rounded-md bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900 inline-flex items-center gap-1 disabled:opacity-50"
        >
          <Check size={12} aria-hidden="true" /> Confirm pair
        </button>
        <button
          type="button"
          onClick={() => void onReject(effectiveLocalId, effectiveRemoteId)}
          disabled={busy !== null || !effectiveRemoteId}
          className="px-2.5 py-1 text-xs rounded-md border border-zinc-200 dark:border-zinc-700 inline-flex items-center gap-1 disabled:opacity-50"
        >
          <X size={12} aria-hidden="true" /> Reject pair
        </button>
      </div>
    </div>
  );
}

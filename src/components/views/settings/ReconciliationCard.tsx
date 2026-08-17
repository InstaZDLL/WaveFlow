import { useCallback, useEffect, useState } from "react";
import { Check, Link2, Loader2, RefreshCw, Unlink, X } from "lucide-react";
import {
  remoteConfirmReconciliation,
  remoteGetStatus,
  remoteListReconciliationLinks,
  remoteReconcileScan,
  remoteRejectReconciliation,
  remoteRemoveReconciliationLink,
  remoteSetReconciliationPreference,
  type MatchCandidateGroup,
  type ReconciliationLink,
  type ReconciliationReport,
} from "../../../lib/tauri/remoteServer";

/** M5 identity links. Kept beside the connection card because it only makes
 * sense for a bootstrapped native remote source. */
export function ReconciliationCard() {
  const [visible, setVisible] = useState(false);
  const [links, setLinks] = useState<ReconciliationLink[]>([]);
  const [report, setReport] = useState<ReconciliationReport | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refreshLinks = useCallback(async () => {
    setLinks(await remoteListReconciliationLinks());
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
    try {
      const next = await remoteReconcileScan();
      setReport(next);
      await refreshLinks();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(null);
    }
  }, [refreshLinks]);

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
            <button
              type="button"
              onClick={() => void scan()}
              disabled={busy !== null}
              className="shrink-0 px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 inline-flex items-center gap-1.5 disabled:opacity-50"
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
                  className="flex items-center gap-3 rounded-lg border border-zinc-200 dark:border-zinc-700 px-3 py-2"
                >
                  <div className="min-w-0 flex-1">
                    <p className="text-sm text-zinc-800 dark:text-zinc-100 truncate">
                      {link.local_title}
                    </p>
                    <p className="text-xs text-zinc-500 truncate">
                      {link.remote_title ?? link.remote_track_id}
                    </p>
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

          {error && (
            <p className="text-xs text-red-600 dark:text-red-400 break-words">
              {error}
            </p>
          )}
        </div>
      </div>
    </div>
  );
}

function ReportSummary({ report }: { report: ReconciliationReport }) {
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

  return (
    <div className="rounded-lg border border-amber-200 dark:border-amber-900/60 bg-amber-50/40 dark:bg-amber-950/10 p-3 space-y-2">
      <p className="text-xs font-medium text-amber-800 dark:text-amber-300">
        Identical copies need confirmation
      </p>
      <div className="grid grid-cols-1 xl:grid-cols-2 gap-2">
        <select
          value={localId}
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
          value={remoteId}
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
          onClick={() => void onConfirm(localId, remoteId)}
          disabled={busy !== null || !remoteId}
          className="px-2.5 py-1 text-xs rounded-md bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900 inline-flex items-center gap-1 disabled:opacity-50"
        >
          <Check size={12} aria-hidden="true" /> Confirm pair
        </button>
        <button
          type="button"
          onClick={() => void onReject(localId, remoteId)}
          disabled={busy !== null || !remoteId}
          className="px-2.5 py-1 text-xs rounded-md border border-zinc-200 dark:border-zinc-700 inline-flex items-center gap-1 disabled:opacity-50"
        >
          <X size={12} aria-hidden="true" /> Reject pair
        </button>
      </div>
    </div>
  );
}

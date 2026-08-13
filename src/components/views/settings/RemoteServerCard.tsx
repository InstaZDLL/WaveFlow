import { useCallback, useEffect, useState } from "react";
import { CloudOff, Loader2, RefreshCw, Server, Unplug } from "lucide-react";
import {
  remoteBeginLogin,
  remoteCreatePlaylist,
  remoteDeletePlaylist,
  remoteDetectServer,
  remoteForgetServer,
  remoteGetOverview,
  remoteGetStatus,
  remoteListPlaylists,
  remoteSignOut,
  remoteSyncNow,
  type RemoteOverview,
  type RemotePlaylistSummary,
  type RemoteProbeResult,
  type RemoteStatus,
} from "../../../lib/tauri/remoteServer";

/**
 * Settings → remote server binding (RFC-005).
 *
 * ## Deliberately not localized
 *
 * Every other string in this app carries a key in all seventeen locale
 * files. This card does not, and that is a decision rather than an
 * oversight: the `sync_v2` Cargo feature is off by default, so no
 * shipped build can render this at all. Translating copy that no user
 * can reach — and that will still change as the feature grows — would
 * put ~255 unreviewed strings into files that had a native-speaker
 * pass. The keys land when the feature ships and the wording settles.
 *
 * Until then this is a developer surface: it exists so the protocol can
 * be exercised and watched by a human, which is the one thing unit
 * tests cannot do for it.
 *
 * ## It hides itself when the feature is absent
 *
 * TypeScript cannot see a Cargo feature, so the card probes for its own
 * backend: if `remote_get_status` is not a registered command, the
 * whole thing renders nothing rather than showing a broken panel.
 */
export function RemoteServerCard() {
  const [available, setAvailable] = useState<boolean | null>(null);
  const [status, setStatus] = useState<RemoteStatus | null>(null);
  const [overview, setOverview] = useState<RemoteOverview | null>(null);
  const [playlists, setPlaylists] = useState<RemotePlaylistSummary[]>([]);
  const [urlDraft, setUrlDraft] = useState("");
  const [probe, setProbe] = useState<RemoteProbeResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [newPlaylist, setNewPlaylist] = useState("");
  const [busy, setBusy] = useState<
    null | "probe" | "login" | "sync" | "forget" | "write"
  >(null);

  const refresh = useCallback(async () => {
    const [next, counts, lists] = await Promise.all([
      remoteGetStatus(),
      remoteGetOverview(),
      remoteListPlaylists(),
    ]);
    setStatus(next);
    setOverview(counts);
    setPlaylists(lists);
    if (next.server_url) setUrlDraft(next.server_url);
  }, []);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        await remoteGetStatus();
        if (cancelled) return;
        setAvailable(true);
        await refresh();
      } catch {
        // The command is not registered: this build has sync_v2 off.
        if (!cancelled) setAvailable(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [refresh]);

  const run = useCallback(
    async (kind: NonNullable<typeof busy>, action: () => Promise<unknown>) => {
      setBusy(kind);
      setError(null);
      try {
        await action();
        await refresh();
      } catch (err) {
        setError(String(err));
      } finally {
        setBusy(null);
      }
    },
    [refresh],
  );

  if (available !== true) return null;

  const signedIn = status?.signed_in ?? false;

  return (
    <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
      <div className="flex items-start space-x-4">
        <Server size={20} className="text-zinc-400 mt-0.5" aria-hidden="true" />
        <div className="flex-1 min-w-0 space-y-4">
          <div>
            <h3 className="text-sm font-medium text-zinc-900 dark:text-zinc-100">
              Remote server
            </h3>
            <p className="text-xs text-zinc-500 dark:text-zinc-400">
              Developer surface — not localized while the feature is
              off by default.
            </p>
          </div>

          <div className="flex gap-2">
            <input
              type="url"
              value={urlDraft}
              onChange={(event) => setUrlDraft(event.target.value)}
              placeholder="https://music.example"
              spellCheck={false}
              disabled={signedIn}
              className="flex-1 min-w-0 px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 disabled:opacity-50"
              aria-label="Server URL"
            />
            <button
              type="button"
              onClick={() =>
                void run("probe", async () =>
                  setProbe(await remoteDetectServer(urlDraft)),
                )
              }
              disabled={busy !== null || !urlDraft.trim() || signedIn}
              className="px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 disabled:opacity-50"
            >
              {busy === "probe" ? <Spinner /> : "Identify"}
            </button>
          </div>

          {probe && !signedIn && (
            <p className="text-xs text-zinc-600 dark:text-zinc-300">
              {probe.server_type ?? "unknown"}
              {probe.server_version ? ` ${probe.server_version}` : ""} —{" "}
              {probe.supports_sync
                ? "native, signs in through the browser"
                : "Subsonic protocol only; no journal, and no sign-in flow here yet"}
            </p>
          )}

          <div className="flex flex-wrap items-center gap-2">
            {!signedIn ? (
              <button
                type="button"
                onClick={() =>
                  void run("login", () => remoteBeginLogin(urlDraft))
                }
                disabled={
                  busy !== null || !urlDraft.trim() || probe?.supports_sync === false
                }
                className="px-3 py-1.5 text-sm rounded-lg bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900 disabled:opacity-50"
              >
                {busy === "login" ? <Spinner /> : "Sign in with browser"}
              </button>
            ) : (
              <>
                <span className="text-sm text-zinc-700 dark:text-zinc-200">
                  {status?.username ?? "signed in"}
                </span>
                <button
                  type="button"
                  onClick={() => void run("sync", remoteSyncNow)}
                  disabled={busy !== null}
                  className="px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 inline-flex items-center gap-1.5 disabled:opacity-50"
                >
                  {busy === "sync" ? (
                    <Spinner />
                  ) : (
                    <RefreshCw size={14} aria-hidden="true" />
                  )}
                  Sync now
                </button>
                <button
                  type="button"
                  onClick={() => void run("login", remoteSignOut)}
                  disabled={busy !== null}
                  className="px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 inline-flex items-center gap-1.5 disabled:opacity-50"
                >
                  <Unplug size={14} aria-hidden="true" />
                  Sign out
                </button>
              </>
            )}
            {status?.server_url && (
              <button
                type="button"
                onClick={() => void run("forget", remoteForgetServer)}
                disabled={busy !== null}
                className="px-3 py-1.5 text-sm rounded-lg text-red-600 dark:text-red-400 border border-red-200 dark:border-red-900/50 inline-flex items-center gap-1.5 disabled:opacity-50"
                // Destructive in a way signing out is not: this one
                // discards changes that never reached the server.
                title="Discards the cached library and any change made offline that never reached the server"
              >
                <CloudOff size={14} aria-hidden="true" />
                Forget server
              </button>
            )}
          </div>

          {status && !status.bootstrapped && signedIn && (
            <p className="text-xs text-amber-600 dark:text-amber-400">
              Never synchronized yet — the first pass downloads a full
              snapshot.
            </p>
          )}

          {error && (
            <p className="text-xs text-red-600 dark:text-red-400 break-words">
              {error}
            </p>
          )}

          {overview && signedIn && <Counts overview={overview} />}

          {signedIn && (
            <div className="flex gap-2">
              {/* Exercises the write path end to end: the playlist
                  appears at once with a local identifier, travels on the
                  next drain, and comes back named by the server. */}
              <input
                type="text"
                value={newPlaylist}
                onChange={(event) => setNewPlaylist(event.target.value)}
                placeholder="New playlist name"
                className="flex-1 min-w-0 px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900"
                aria-label="New playlist name"
              />
              <button
                type="button"
                onClick={() =>
                  void run("write", async () => {
                    await remoteCreatePlaylist(newPlaylist);
                    setNewPlaylist("");
                  })
                }
                disabled={busy !== null || !newPlaylist.trim()}
                className="px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 disabled:opacity-50"
              >
                {busy === "write" ? <Spinner /> : "Create"}
              </button>
            </div>
          )}

          {playlists.length > 0 && (
            <ul className="text-xs text-zinc-600 dark:text-zinc-300 space-y-0.5">
              {playlists.slice(0, 8).map((playlist) => (
                <li key={playlist.id} className="flex items-center gap-2">
                  <span className="truncate flex-1">
                    {playlist.name} — {playlist.track_count} tracks
                    {playlist.pending_creation && " (not sent yet)"}
                  </span>
                  <button
                    type="button"
                    onClick={() =>
                      void run("write", () => remoteDeletePlaylist(playlist.id))
                    }
                    disabled={busy !== null}
                    className="text-zinc-400 hover:text-red-600 dark:hover:text-red-400 disabled:opacity-50"
                    aria-label={`Delete ${playlist.name}`}
                  >
                    ×
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </div>
  );
}

function Counts({ overview }: { overview: RemoteOverview }) {
  const entries: [string, number][] = [
    ["playlists", overview.playlists],
    ["favourites", overview.favorites],
    ["ratings", overview.ratings],
    ["history", overview.history],
    ["shares", overview.shares],
    ["queue", overview.queue_tracks],
    ["cached tracks", overview.cached_tracks],
  ];
  return (
    <div className="space-y-1">
      <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-zinc-500 dark:text-zinc-400">
        {entries.map(([label, value]) => (
          <span key={label}>
            <span className="tabular-nums text-zinc-700 dark:text-zinc-200">
              {value}
            </span>{" "}
            {label}
          </span>
        ))}
      </div>
      {/* Awaiting metadata is normal right after a catch-up and clears
          on the next pass, so it is informational. Pending changes are
          the same. A permanent failure is neither: nothing will retry
          it, so it gets the only alarming colour on this card. */}
      {overview.tracks_awaiting_metadata > 0 && (
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          {overview.tracks_awaiting_metadata} tracks awaiting metadata —
          clears on the next pass.
        </p>
      )}
      {overview.pending_changes > 0 && (
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          {overview.pending_changes} local changes waiting to be sent.
        </p>
      )}
      {overview.failed_changes > 0 && (
        <p className="text-xs text-red-600 dark:text-red-400">
          {overview.failed_changes} local changes were refused and will
          not be retried.
        </p>
      )}
    </div>
  );
}

function Spinner() {
  return <Loader2 size={14} className="animate-spin" aria-hidden="true" />;
}

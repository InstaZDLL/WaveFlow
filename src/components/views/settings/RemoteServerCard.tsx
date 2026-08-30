import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { CloudOff, Loader2, RefreshCw, Server, Unplug } from "lucide-react";
import {
  remoteBeginLogin,
  remoteDetectServer,
  remoteForgetServer,
  remoteGetOverview,
  remoteGetStatus,
  remoteSignOut,
  remoteSyncNow,
  type RemoteOverview,
  type RemoteProbeResult,
  type RemoteStatus,
} from "../../../lib/tauri/remoteServer";
import { notifyRemoteChanged } from "../../../hooks/useRemoteSource";

/**
 * Settings → remote server binding (RFC-005).
 *
 * ## Connection only
 *
 * This card does one thing: bind a profile to a server and manage that
 * binding — identify, sign in, sync, sign out, forget. The library it
 * exposes (playlists and their tracks, playback, create / rename /
 * delete) lives in the main UI: the server's playlists sit in the one
 * playlist list and open in `PlaylistView`, the same view a local one
 * opens in. Nothing about browsing or playing remote music belongs in
 * Settings.
 *
 * ## Localized under `remote.*`
 *
 * `sync_v2` now ships in the default feature set, so this surface is
 * reachable in a released build and is localized across every locale —
 * the whole remote surface shares the self-contained `remote.*` i18n
 * namespace.
 *
 * ## It hides itself when the feature is absent
 *
 * TypeScript cannot see a Cargo feature, so the card probes for its own
 * backend: if `remote_get_status` is not a registered command, the whole
 * thing renders nothing rather than showing a broken panel.
 */
export function RemoteServerCard() {
  const { t } = useTranslation();
  const [available, setAvailable] = useState<boolean | null>(null);
  const [status, setStatus] = useState<RemoteStatus | null>(null);
  const [overview, setOverview] = useState<RemoteOverview | null>(null);
  const [urlDraft, setUrlDraft] = useState("");
  const [probe, setProbe] = useState<RemoteProbeResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<
    null | "probe" | "login" | "signout" | "sync" | "forget"
  >(null);
  const mountedRef = useRef(false);

  const refresh = useCallback(async () => {
    // Status is availability-critical; the overview is only counts. Read
    // them independently so a transient overview failure never blanks the
    // card, and neither call clobbers state after unmount.
    const next = await remoteGetStatus();
    if (!mountedRef.current) return;
    setStatus(next);
    if (next.server_url) setUrlDraft(next.server_url);
    try {
      const counts = await remoteGetOverview();
      if (mountedRef.current) setOverview(counts);
    } catch {
      // Informational only — keep the last counts.
    }
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    void (async () => {
      try {
        // Availability hinges on this one call alone: if it rejects, the
        // command is not registered (sync_v2 off) and the card hides.
        await remoteGetStatus();
        if (!mountedRef.current) return;
        setAvailable(true);
        try {
          await refresh();
        } catch {
          // A transient status failure right after the probe must not flip
          // availability back off — the command clearly exists.
        }
      } catch {
        if (mountedRef.current) setAvailable(false);
      }
    })();
    return () => {
      mountedRef.current = false;
    };
  }, [refresh]);

  const run = useCallback(
    async (kind: NonNullable<typeof busy>, action: () => Promise<unknown>) => {
      setBusy(kind);
      setError(null);
      try {
        await action();
        await refresh();
        // The sidebar remote-source section reads the same binding.
        notifyRemoteChanged();
      } catch (err) {
        if (mountedRef.current) setError(String(err));
      } finally {
        if (mountedRef.current) setBusy(null);
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
              {t("remote.server.title")}
            </h3>
            <p className="text-xs text-zinc-500 dark:text-zinc-400">
              {t("remote.server.subtitle")}
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
              aria-label={t("remote.server.urlLabel")}
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
              {busy === "probe" ? <Spinner /> : t("remote.server.identify")}
            </button>
          </div>

          {probe && !signedIn && (
            <p className="text-xs text-zinc-600 dark:text-zinc-300">
              {probe.server_type ?? t("remote.server.unknownType")}
              {probe.server_version ? ` ${probe.server_version}` : ""} —{" "}
              {probe.supports_sync
                ? t("remote.server.probeNative")
                : t("remote.server.probeSubsonic")}
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
                {busy === "login" ? (
                  <Spinner />
                ) : (
                  t("remote.server.signInBrowser")
                )}
              </button>
            ) : (
              <>
                <span className="text-sm text-zinc-700 dark:text-zinc-200">
                  {status?.username ?? t("remote.server.signedInFallback")}
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
                  {t("remote.server.syncNow")}
                </button>
                <button
                  type="button"
                  onClick={() => void run("signout", remoteSignOut)}
                  disabled={busy !== null}
                  className="px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 inline-flex items-center gap-1.5 disabled:opacity-50"
                >
                  {busy === "signout" ? (
                    <Spinner />
                  ) : (
                    <Unplug size={14} aria-hidden="true" />
                  )}
                  {t("remote.server.signOut")}
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
                title={t("remote.server.forgetTitle")}
              >
                <CloudOff size={14} aria-hidden="true" />
                {t("remote.server.forget")}
              </button>
            )}
          </div>

          {status && !status.bootstrapped && signedIn && (
            <p className="text-xs text-amber-600 dark:text-amber-400">
              {t("remote.server.neverSynced")}
            </p>
          )}

          {error && (
            <p className="text-xs text-red-600 dark:text-red-400 break-words">
              {error}
            </p>
          )}

          {overview && signedIn && <Counts overview={overview} />}
        </div>
      </div>
    </div>
  );
}

function Counts({ overview }: { overview: RemoteOverview }) {
  const { t } = useTranslation();
  // The number is rendered separately (tabular-nums, its own colour), so the
  // label carries no `{{count}}` — `count` only drives plural selection, which
  // keeps "1 playlist" / "2 playlists" correct in every language.
  const counts: [string, number][] = [
    ["playlists", overview.playlists],
    ["favorites", overview.favorites],
    ["ratings", overview.ratings],
    ["history", overview.history],
    ["shares", overview.shares],
    ["queue", overview.queue_tracks],
    ["cachedTracks", overview.cached_tracks],
  ];
  const entries = counts.map(([key, value]) => ({
    key,
    label: t(`remote.server.counts.${key}`, { count: value }),
    value,
  }));
  return (
    <div className="space-y-1">
      <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-zinc-500 dark:text-zinc-400">
        {entries.map(({ key, label, value }) => (
          <span key={key}>
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
          {t("remote.server.awaitingMetadata", {
            count: overview.tracks_awaiting_metadata,
          })}
        </p>
      )}
      {overview.pending_changes > 0 && (
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          {t("remote.server.pendingChanges", {
            count: overview.pending_changes,
          })}
        </p>
      )}
      {overview.failed_changes > 0 && (
        <p className="text-xs text-red-600 dark:text-red-400">
          {t("remote.server.failedChanges", { count: overview.failed_changes })}
        </p>
      )}
    </div>
  );
}

function Spinner() {
  return <Loader2 size={14} className="animate-spin" aria-hidden="true" />;
}

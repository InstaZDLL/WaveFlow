import { useCallback, useEffect, useState } from "react";
import {
  remoteGetStatus,
  remoteListPlaylists,
  type RemotePlaylistSummary,
} from "../lib/tauri/remoteServer";

export interface RemoteSourceState {
  /**
   * True only when the `sync_v2` backend is compiled in **and** a server
   * is bound and signed in. A stock build (feature off) makes
   * `remote_get_status` an unregistered command, which rejects and lands
   * us on `false` — the whole sidebar section then renders nothing, the
   * same self-hiding contract as {@link RemoteServerCard}.
   */
  available: boolean;
  /** Host of the bound server, shown as the sidebar section header. */
  serverName: string | null;
  playlists: RemotePlaylistSummary[];
  refresh: () => void;
}

/**
 * Drives the "Remote source" sidebar section and keeps it in step with
 * the rest of the UI. Any mutation of remote data (create / rename /
 * delete a remote playlist, sign in / out) should dispatch a
 * `waveflow:remote-changed` window event; every consumer refreshes on it
 * so the sidebar, the settings card and the open view never drift apart.
 */
export function useRemoteSource(): RemoteSourceState {
  const [available, setAvailable] = useState(false);
  const [serverName, setServerName] = useState<string | null>(null);
  const [playlists, setPlaylists] = useState<RemotePlaylistSummary[]>([]);

  const refresh = useCallback(() => {
    void (async () => {
      try {
        const status = await remoteGetStatus();
        if (!status.signed_in) {
          setAvailable(false);
          setServerName(null);
          setPlaylists([]);
          return;
        }
        setAvailable(true);
        setServerName(hostOf(status.server_url) ?? status.username ?? "Remote");
        try {
          setPlaylists(await remoteListPlaylists());
        } catch {
          // Signed in but the list call failed (offline, transient): keep
          // the section visible with whatever we last had rather than
          // making it vanish under the user.
        }
      } catch {
        // `remote_get_status` is not a registered command: sync_v2 off.
        setAvailable(false);
        setServerName(null);
        setPlaylists([]);
      }
    })();
  }, []);

  useEffect(() => {
    refresh();
    const onChange = () => refresh();
    window.addEventListener("waveflow:remote-changed", onChange);
    return () =>
      window.removeEventListener("waveflow:remote-changed", onChange);
  }, [refresh]);

  return { available, serverName, playlists, refresh };
}

/** Signal every remote-source consumer to re-read. */
export function notifyRemoteChanged() {
  window.dispatchEvent(new Event("waveflow:remote-changed"));
}

function hostOf(url: string | null): string | null {
  if (!url) return null;
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

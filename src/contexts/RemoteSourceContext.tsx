import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { remoteGetStatus, remoteListPlaylists } from "../lib/tauri/remoteServer";
import {
  RemoteSourceContext,
  type RemoteSourceState,
} from "../hooks/useRemoteSource";

/**
 * Single owner of the "Remote source" state (RFC-005). Holds the status +
 * playlist list and the one `waveflow:remote-changed` listener, so every
 * consumer — the sidebar section, the create-playlist modal — reads the
 * same snapshot and one event triggers one refresh, not one per mounted
 * `useRemoteSource` caller.
 *
 * Mounted unconditionally: in a stock build `remote_get_status` is not a
 * registered command, so `available` stays `false` and every consumer
 * renders nothing.
 */
export function RemoteSourceProvider({ children }: { children: ReactNode }) {
  const [available, setAvailable] = useState(false);
  const [serverName, setServerName] = useState<string | null>(null);
  const [playlists, setPlaylists] = useState<RemoteSourceState["playlists"]>([]);
  // Monotonic token so a slow pass can't overwrite a newer one: bursts of
  // `waveflow:remote-changed` fire overlapping refreshes, and without this
  // an older status/list response landing last would clobber fresher data.
  const seqRef = useRef(0);
  // Once we've resolved as available, sync_v2 is compiled in — so a later
  // status failure is transient (never "command absent"), and must not
  // make the section vanish under the user.
  const everAvailableRef = useRef(false);

  const refresh = useCallback(() => {
    const seq = ++seqRef.current;
    const isCurrent = () => seq === seqRef.current;
    void (async () => {
      try {
        const status = await remoteGetStatus();
        if (!isCurrent()) return;
        if (!status.signed_in) {
          setAvailable(false);
          setServerName(null);
          setPlaylists([]);
          return;
        }
        everAvailableRef.current = true;
        setAvailable(true);
        setServerName(hostOf(status.server_url) ?? status.username ?? "Remote");
        try {
          const list = await remoteListPlaylists();
          if (isCurrent()) setPlaylists(list);
        } catch {
          // Signed in but the list call failed (offline, transient): keep
          // the section visible with whatever we last had rather than
          // making it vanish under the user.
        }
      } catch {
        if (!isCurrent()) return;
        // Only hide when we've never resolved: the command may be absent
        // (sync_v2 off). Once we've been available, this is a transient
        // status failure — keep the last state instead of flipping off.
        if (!everAvailableRef.current) {
          setAvailable(false);
          setServerName(null);
          setPlaylists([]);
        }
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

  const value = useMemo<RemoteSourceState>(
    () => ({ available, serverName, playlists, refresh }),
    [available, serverName, playlists, refresh],
  );

  return (
    <RemoteSourceContext.Provider value={value}>
      {children}
    </RemoteSourceContext.Provider>
  );
}

function hostOf(url: string | null): string | null {
  if (!url) return null;
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

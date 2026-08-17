import { createContext, useContext } from "react";
import type { RemotePlaylistSummary } from "../lib/tauri/remoteServer";

export interface RemoteSourceState {
  /**
   * True only when the `sync_v2` backend is compiled in **and** a server
   * is bound and signed in. A stock build (feature off) makes
   * `remote_get_status` an unregistered command, which rejects and lands
   * us on `false` — the whole sidebar section then renders nothing, the
   * same self-hiding contract as `RemoteServerCard`.
   */
  available: boolean;
  /** Host of the bound server, shown as the sidebar section header. */
  serverName: string | null;
  playlists: RemotePlaylistSummary[];
  refresh: () => void;
}

/**
 * Shared remote-source state, owned by `RemoteSourceProvider`. Lives here
 * (with the hook) rather than in the provider file so the provider module
 * can stay a components-only file for fast refresh — the same split as
 * `PlayerContext` / `usePlayer`.
 */
export const RemoteSourceContext = createContext<RemoteSourceState | null>(
  null,
);

/**
 * Read the shared remote-source state — one `waveflow:remote-changed` event
 * refreshes every consumer once, since the provider owns the single
 * listener. Falls back to an inert snapshot outside the provider so a stray
 * consumer degrades to "nothing here" rather than throwing; the section is
 * optional by design.
 */
export function useRemoteSource(): RemoteSourceState {
  return useContext(RemoteSourceContext) ?? INERT;
}

const INERT: RemoteSourceState = {
  available: false,
  serverName: null,
  playlists: [],
  refresh: () => {},
};

/** Signal every remote-source consumer to re-read. */
export function notifyRemoteChanged() {
  window.dispatchEvent(new Event("waveflow:remote-changed"));
}

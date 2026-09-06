import { createPlaylist, type Playlist } from "./tauri/playlist";
import { remoteCreatePlaylist } from "./tauri/remoteServer";
import { notifyRemoteChanged } from "../hooks/useRemoteSource";

/** What `CreatePlaylistModal` hands back on submit. */
export interface CreatePlaylistModalData {
  name: string;
  description: string;
  colorId: string;
  iconId: string;
  /** "Also create on <server>" — only ever true when a server is linked. */
  alsoOnServer: boolean;
}

/**
 * Create a playlist the way the modal promises, mirror included.
 *
 * The modal renders its "also create on the server" checkbox from its own
 * `useRemoteSource()`, so the box appears wherever the modal is mounted —
 * nine places. Honouring it was written once, in the sidebar, and the other
 * eight built the playlist locally and dropped the flag: the box was checked,
 * nothing reached the server, and the failure had nothing to report because
 * no call was ever made. Putting the mirror behind the same function as the
 * local create is what stops the tenth caller from forgetting it again.
 *
 * The mirror is deliberately best-effort and unawaited: the local playlist is
 * what the user watches land, and a server hiccup must not sink it. It is
 * also the reason this returns as soon as the local row exists.
 */
export async function createPlaylistFromModal(
  data: CreatePlaylistModalData,
): Promise<Playlist> {
  const created = await createPlaylist({
    name: data.name,
    description: data.description || null,
    color_id: data.colorId,
    icon_id: data.iconId,
  });
  if (data.alsoOnServer) {
    void remoteCreatePlaylist(data.name)
      .then(() => notifyRemoteChanged())
      .catch((err) =>
        console.error("[playlist] mirror to server failed", err),
      );
  }
  return created;
}

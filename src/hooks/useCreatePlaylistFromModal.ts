import { useCallback } from "react";
import { usePlaylist } from "./usePlaylist";
import type { Playlist } from "../lib/tauri/playlist";
import { remoteCreatePlaylist } from "../lib/tauri/remoteServer";
import { notifyRemoteChanged } from "./useRemoteSource";

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
 * eight built the playlist locally and dropped the flag: the box was ticked,
 * nothing reached the server, and the failure had nothing to report because
 * no call was ever made. Putting the mirror behind the same function as the
 * local create is what stops the tenth caller from forgetting it again.
 *
 * A hook rather than a plain function, because the local half must go through
 * `usePlaylist()`: that is what refreshes the playlist context, and a raw
 * `createPlaylist` would leave every caller's list stale until something else
 * happened to reload it.
 *
 * The mirror is deliberately best-effort and unawaited: the local playlist is
 * what the user watches land, and a server hiccup must not sink it.
 */
export function useCreatePlaylistFromModal() {
  const { createPlaylist } = usePlaylist();
  return useCallback(
    async (data: CreatePlaylistModalData): Promise<Playlist> => {
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
    },
    [createPlaylist],
  );
}

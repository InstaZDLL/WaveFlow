import { useCallback, useState } from "react";
import { CreatePlaylistModal } from "../components/common/CreatePlaylistModal";
import { useCreatePlaylistFromModal } from "./useCreatePlaylistFromModal";
import { usePlaylist } from "./usePlaylist";

/**
 * "New playlist" for a set of tracks the caller already holds: a track's
 * context menu, the selection bar. Owns the create dialog, so a surface
 * that offers the action only calls `open(ids)` and mounts `render()`.
 *
 * The playlist is created empty and the tracks are added once it exists.
 * That second step was the caller's to remember, and every caller that
 * only opened a shared dialog forgot it: the track menu in the detail
 * views, the player surfaces and the library, and the selection bar
 * everywhere, all created an empty playlist. Holding the ids next to the
 * dialog that creates the playlist is what keeps the next surface from
 * forgetting it again.
 *
 * The ids are taken when the dialog opens and counted once each — the
 * add inserts a track one time however often it was listed — so the
 * preview names what the playlist receives, and a selection that changes
 * behind the dialog does not change what lands. A failed add leaves the
 * playlist created and still returns it: the dialog then closes instead
 * of offering a retry that would create a second one. A failed create
 * keeps the ids, so trying again still fills the playlist.
 */
export function useCreatePlaylistWithTracks() {
  const createFromModal = useCreatePlaylistFromModal();
  const { addTracksToPlaylist } = usePlaylist();
  // Kept after a close so the dialog's closing animation keeps its count;
  // the next `open` replaces them.
  const [trackIds, setTrackIds] = useState<number[]>([]);
  const [isOpen, setIsOpen] = useState(false);

  const open = useCallback((ids: number[]) => {
    setTrackIds([...new Set(ids)]);
    setIsOpen(true);
  }, []);

  const close = useCallback(() => setIsOpen(false), []);

  const render = useCallback(
    () => (
      <CreatePlaylistModal
        isOpen={isOpen}
        onClose={close}
        incomingTrackCount={trackIds.length}
        onCreate={async (data) => {
          const created = await createFromModal(data);
          if (created?.id != null && trackIds.length > 0) {
            try {
              await addTracksToPlaylist(created.id, trackIds);
            } catch (err) {
              // The playlist exists; returning it keeps the dialog from
              // offering a retry that would create a duplicate.
              console.error(
                "[playlist] add tracks to new playlist failed",
                err,
              );
            }
          }
          return created;
        }}
      />
    ),
    [isOpen, close, trackIds, createFromModal, addTracksToPlaylist],
  );

  return { open, render, isOpen };
}

import {
  useCallback,
  useEffect,
  useState,
  type MouseEvent as ReactMouseEvent,
} from "react";
import { CreatePlaylistModal } from "../components/common/CreatePlaylistModal";
import { useTrackContextMenu } from "./useTrackContextMenu";
import { usePlaylist } from "./usePlaylist";
import { listLikedTrackIds, type Track } from "../lib/tauri/track";

/**
 * Track context menu for the player surfaces (ImmersiveView, QueuePanel)
 * that don't own a track list of their own. Bundles the liked-id lookup
 * and the "create playlist" modal the base [`useTrackContextMenu`] needs,
 * so a caller only wires `onContextMenu={(e) => open(e, track)}` on the
 * row and `{render()}` near its root — reaching "Show in Explorer" and
 * the rest of the menu from the immersive view and the queue (mail
 * reporter request).
 *
 * Liked ids are fetched once on mount and kept in sync through the base
 * hook's `onLikedChanged`; navigation + remove-from-playlist are omitted
 * because these surfaces have no in-view destination for them.
 */
export function usePlayerTrackContextMenu() {
  const { createPlaylist } = usePlaylist();
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());
  const [isCreatePlaylistOpen, setIsCreatePlaylistOpen] = useState(false);

  useEffect(() => {
    let cancelled = false;
    listLikedTrackIds()
      .then((ids) => {
        if (!cancelled) setLikedIds(new Set(ids));
      })
      .catch((err) =>
        console.error("[usePlayerTrackContextMenu] liked fetch failed", err),
      );
    return () => {
      cancelled = true;
    };
  }, []);

  const menu = useTrackContextMenu({
    likedIds,
    onLikedChanged: (trackId, nowLiked) => {
      setLikedIds((prev) => {
        const next = new Set(prev);
        if (nowLiked) next.add(trackId);
        else next.delete(trackId);
        return next;
      });
    },
    onCreatePlaylist: () => setIsCreatePlaylistOpen(true),
  });

  const open = useCallback(
    (event: ReactMouseEvent, track: Track) => menu.open(event, track),
    [menu],
  );

  const render = useCallback(
    () => (
      <>
        {menu.render()}
        <CreatePlaylistModal
          isOpen={isCreatePlaylistOpen}
          onClose={() => setIsCreatePlaylistOpen(false)}
          onCreate={async (data) => {
            try {
              await createPlaylist({
                name: data.name,
                description: data.description || null,
                color_id: data.colorId,
                icon_id: data.iconId,
              });
            } catch (err) {
              console.error(
                "[usePlayerTrackContextMenu] create playlist failed",
                err,
              );
            }
          }}
        />
      </>
    ),
    [menu, isCreatePlaylistOpen, createPlaylist],
  );

  return { open, render };
}

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
} from "react";
import { CreatePlaylistModal } from "../components/common/CreatePlaylistModal";
import { useTrackContextMenu } from "./useTrackContextMenu";
import { usePlaylist } from "./usePlaylist";
import { useProfile } from "./useProfile";
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
 * Liked ids are (re)fetched per active profile and kept in sync through
 * the base hook's `onLikedChanged`; navigation + remove-from-playlist are
 * omitted because these surfaces have no in-view destination for them.
 * The rating action is omitted too: the queue payload these surfaces
 * widen into a `Track` carries no rating, so the submenu would misreport
 * an already-rated track as unrated.
 */
export function usePlayerTrackContextMenu() {
  const { createPlaylist } = usePlaylist();
  const { activeProfile } = useProfile();
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());
  const [isCreatePlaylistOpen, setIsCreatePlaylistOpen] = useState(false);
  // Toggles the user made through `onLikedChanged` while the per-profile
  // fetch was still in flight. Re-applied on top of the fetched base so a
  // late response can't revert a like/unlike the user just performed.
  const likedDeltasRef = useRef<Map<number, boolean>>(new Map());

  useEffect(() => {
    let cancelled = false;
    // Fresh profile → the pending deltas belonged to the old one.
    likedDeltasRef.current = new Map();
    // Drop the previous profile's liked set so its likes can't bleed into
    // this profile's menu during the load window below. A default-unliked
    // display self-corrects: the like action always toggles against real
    // DB state via `toggleLikeTrack`.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLikedIds(new Set());
    // `listLikedTrackIds` is scoped to the active profile pool, so a
    // profile switch must reload; the cancel guard drops a stale
    // profile's response.
    listLikedTrackIds()
      .then((ids) => {
        if (cancelled) return;
        const merged = new Set(ids);
        for (const [id, nowLiked] of likedDeltasRef.current) {
          if (nowLiked) merged.add(id);
          else merged.delete(id);
        }
        setLikedIds(merged);
      })
      .catch((err) =>
        console.error("[usePlayerTrackContextMenu] liked fetch failed", err),
      );
    return () => {
      cancelled = true;
    };
  }, [activeProfile?.id]);

  const menu = useTrackContextMenu({
    likedIds,
    onLikedChanged: (trackId, nowLiked) => {
      likedDeltasRef.current.set(trackId, nowLiked);
      setLikedIds((prev) => {
        const next = new Set(prev);
        if (nowLiked) next.add(trackId);
        else next.delete(trackId);
        return next;
      });
    },
    onCreatePlaylist: () => setIsCreatePlaylistOpen(true),
    enableRating: false,
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

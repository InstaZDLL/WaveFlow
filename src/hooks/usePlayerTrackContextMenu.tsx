import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
} from "react";
import { useTrackContextMenu } from "./useTrackContextMenu";
import { useProfile } from "./useProfile";
import { listLikedTrackIds, type Track } from "../lib/tauri/track";

interface UsePlayerTrackContextMenuArgs {
  /** Show the rating submenu. Off by default: the queue payload the
   *  player surfaces widen into a `Track` carries no rating, so the
   *  submenu would misreport an already-rated track as unrated. The
   *  search dropdown's tracks are library rows and turn it on. */
  enableRating?: boolean;
  /** "Go to album" / "Go to artist", on a surface that can navigate
   *  (the search dropdown); omitted, the entries are not offered. */
  onNavigateToAlbum?: (albumId: number) => void;
  onNavigateToArtist?: (artistId: number) => void;
}

/**
 * Track context menu for the surfaces that don't own a track list of
 * their own — the player surfaces (ImmersiveView, QueuePanel) and the
 * search dropdown. Bundles the liked-id lookup the base
 * [`useTrackContextMenu`] needs (which mounts its own "New playlist"
 * dialog), so a caller only wires `onContextMenu={(e) => open(e, track)}`
 * on the row and `{render()}` near its root — reaching "Show in Explorer"
 * and the rest of the menu from the immersive view and the queue (mail
 * reporter request), and adding a searched track to the queue or a
 * playlist (#1170).
 *
 * Liked ids are fetched per active profile, fetched again at each
 * opening — these surfaces stay mounted while the likes change elsewhere,
 * and a stale set would label the entry "Like" on a liked track — and
 * kept in sync through the base hook's `onLikedChanged`. Remove-from-
 * playlist is never offered: none of these surfaces shows a playlist.
 */
export function usePlayerTrackContextMenu({
  enableRating = false,
  onNavigateToAlbum,
  onNavigateToArtist,
}: UsePlayerTrackContextMenuArgs = {}) {
  const { activeProfile } = useProfile();
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());
  // Toggles the user made through `onLikedChanged` while a fetch was
  // still in flight. Re-applied on top of the fetched base so a late
  // response can't revert a like/unlike the user just performed.
  const likedDeltasRef = useRef<Map<number, boolean>>(new Map());
  // The latest fetch: an older one that answers last is dropped.
  const fetchSeqRef = useRef(0);

  const refreshLiked = useCallback(() => {
    const seq = ++fetchSeqRef.current;
    // A toggle reports through `onLikedChanged` only once the database
    // holds it, so the fetch starting now already reads every delta
    // recorded so far; only the ones that land while it runs remain.
    likedDeltasRef.current = new Map();
    // `listLikedTrackIds` is scoped to the active profile pool.
    listLikedTrackIds()
      .then((ids) => {
        if (seq !== fetchSeqRef.current) return;
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
  }, []);

  useEffect(() => {
    // Drop the previous profile's liked set so its likes can't bleed into
    // this profile's menu during the load window below. A default-unliked
    // display self-corrects: the like action always toggles against real
    // DB state via `toggleLikeTrack`.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLikedIds(new Set());
    refreshLiked();
    return () => {
      // A profile switch: the answer in flight belongs to the old one.
      fetchSeqRef.current += 1;
    };
  }, [activeProfile?.id, refreshLiked]);

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
    onNavigateToAlbum,
    onNavigateToArtist,
    enableRating,
  });

  const open = useCallback(
    (event: ReactMouseEvent, track: Track) => {
      menu.open(event, track);
      refreshLiked();
    },
    [menu, refreshLiked],
  );

  // Same keyboard contract as the library surfaces — see
  // `useTrackContextMenu.openFromKeyboard`. Forwarded rather than
  // reimplemented so these surfaces can't drift from the rest.
  const openFromKeyboard = useCallback(
    (event: ReactKeyboardEvent, track: Track) => {
      const handled = menu.openFromKeyboard(event, track);
      if (handled) refreshLiked();
      return handled;
    },
    [menu, refreshLiked],
  );

  const render = menu.render;

  return { open, openFromKeyboard, render, isOpen: menu.isOpen };
}

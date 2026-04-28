import { useCallback, useState, type MouseEvent as ReactMouseEvent } from "react";
import {
  TrackContextMenu,
  type TrackContextMenuProps,
} from "../components/common/TrackContextMenu";
import { TrackPropertiesModal } from "../components/common/TrackPropertiesModal";
import type { Track } from "../lib/tauri/track";
import { toggleLikeTrack } from "../lib/tauri/track";
import { playerAddToQueue, playerPlayNext } from "../lib/tauri/player";
import { usePlaylist } from "./usePlaylist";
import type { ContextMenuPoint } from "../components/common/ContextMenu";

interface UseTrackContextMenuArgs {
  /** Set of liked track IDs the parent already maintains. */
  likedIds: Set<number>;
  /** Called after a successful like/unlike so the parent can update its set. */
  onLikedChanged: (trackId: number, nowLiked: boolean) => void;
  /** Opens the "create playlist" modal — owned by the parent so it can
   *  share the same modal across other UI affordances. */
  onCreatePlaylist: () => void;
  /** Optional: navigation handlers (omit on screens that don't support it). */
  onNavigateToAlbum?: (albumId: number) => void;
  onNavigateToArtist?: (artistId: number) => void;
  /** When set, the menu shows "Remove from this playlist" pointing at it. */
  currentPlaylistId?: number | null;
  onRemoveFromPlaylist?: (playlistId: number, trackId: number) => void;
}

/**
 * Bundles trigger + render for the Spotify-style track context menu.
 * Keeps every view's per-row code to a single `onContextMenu` handler
 * and a single `{render()}` call near its root.
 *
 * The like/queue/add-to-playlist actions are wired internally so views
 * don't repeat the tauri invocations or error handling. Navigation and
 * remove-from-playlist stay caller-controlled because they depend on
 * the surrounding view's state.
 */
export function useTrackContextMenu({
  likedIds,
  onLikedChanged,
  onCreatePlaylist,
  onNavigateToAlbum,
  onNavigateToArtist,
  currentPlaylistId,
  onRemoveFromPlaylist,
}: UseTrackContextMenuArgs) {
  const { playlists, addTracksToPlaylist } = usePlaylist();
  const [state, setState] = useState<{
    point: ContextMenuPoint;
    track: Track;
  } | null>(null);
  // Track currently shown in the Properties modal — `null` means
  // the modal is closed. Keeping it separate from the context-menu
  // state means closing the menu doesn't immediately dismiss the
  // dialog the user just opened.
  const [propertiesTrack, setPropertiesTrack] = useState<Track | null>(null);

  const open = useCallback((event: ReactMouseEvent, track: Track) => {
    event.preventDefault();
    event.stopPropagation();
    setState({
      point: { x: event.clientX, y: event.clientY },
      track,
    });
  }, []);

  const close = useCallback(() => setState(null), []);

  const handleAddToPlaylist: TrackContextMenuProps["onAddToPlaylist"] =
    useCallback(
      async (playlistId, trackId) => {
        try {
          await addTracksToPlaylist(playlistId, [trackId]);
        } catch (err) {
          console.error("[trackContextMenu] add to playlist failed", err);
        }
      },
      [addTracksToPlaylist],
    );

  const handlePlayNext = useCallback((trackId: number) => {
    playerPlayNext([trackId]).catch((err) =>
      console.error("[trackContextMenu] play next failed", err),
    );
  }, []);

  const handleAddToQueue = useCallback((trackId: number) => {
    playerAddToQueue([trackId]).catch((err) =>
      console.error("[trackContextMenu] add to queue failed", err),
    );
  }, []);

  const handleToggleLike = useCallback(
    async (trackId: number) => {
      try {
        const nowLiked = await toggleLikeTrack(trackId);
        onLikedChanged(trackId, nowLiked);
      } catch (err) {
        console.error("[trackContextMenu] toggle like failed", err);
      }
    },
    [onLikedChanged],
  );

  const handleShowProperties = useCallback((track: Track) => {
    setPropertiesTrack(track);
  }, []);

  const closeProperties = useCallback(() => setPropertiesTrack(null), []);

  const render = useCallback(() => {
    return (
      <>
        {state != null && (
          <TrackContextMenu
            point={state.point}
            track={state.track}
            playlists={playlists}
            isLiked={likedIds.has(state.track.id)}
            currentPlaylistId={currentPlaylistId ?? null}
            onClose={close}
            onPlayNext={handlePlayNext}
            onAddToQueue={handleAddToQueue}
            onAddToPlaylist={handleAddToPlaylist}
            onCreatePlaylist={onCreatePlaylist}
            onToggleLike={handleToggleLike}
            onRemoveFromPlaylist={onRemoveFromPlaylist}
            onNavigateToAlbum={onNavigateToAlbum}
            onNavigateToArtist={onNavigateToArtist}
            onShowProperties={handleShowProperties}
          />
        )}
        <TrackPropertiesModal
          key={propertiesTrack?.id ?? "none"}
          track={propertiesTrack}
          onClose={closeProperties}
        />
      </>
    );
  }, [
    state,
    playlists,
    likedIds,
    currentPlaylistId,
    close,
    handlePlayNext,
    handleAddToQueue,
    handleAddToPlaylist,
    onCreatePlaylist,
    handleToggleLike,
    onRemoveFromPlaylist,
    onNavigateToAlbum,
    onNavigateToArtist,
    handleShowProperties,
    propertiesTrack,
    closeProperties,
  ]);

  return { open, close, render };
}

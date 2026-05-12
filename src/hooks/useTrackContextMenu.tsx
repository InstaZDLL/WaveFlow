import {
  useCallback,
  useState,
  type MouseEvent as ReactMouseEvent,
} from "react";
import {
  TrackContextMenu,
  type TrackContextMenuProps,
} from "../components/common/TrackContextMenu";
import { TrackPropertiesModal } from "../components/common/TrackPropertiesModal";
import { BatchTagEditModal } from "../components/common/BatchTagEditModal";
import type { Track } from "../lib/tauri/track";
import { setTrackRating, toggleLikeTrack } from "../lib/tauri/track";
import {
  playerAddToQueue,
  playerPlayNext,
  playerPlayTracks,
} from "../lib/tauri/player";
import { startRadio } from "../lib/tauri/radio";
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
  /** Set of currently-selected track IDs (multi-select). When the
   * right-clicked track is part of this set AND it has 2+ entries,
   * a "Edit tags for N tracks…" item appears that opens the batch
   * editor. Omit on views without multi-select. */
  selectedTrackIds?: number[];
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
  selectedTrackIds,
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
  // IDs currently shown in the batch tag editor — `null` means the
  // modal is closed. Independent of the context-menu state so closing
  // the menu doesn't dismiss a modal the user just opened.
  const [batchEditIds, setBatchEditIds] = useState<number[] | null>(null);

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

  const handleStartRadio = useCallback(async (trackId: number) => {
    try {
      const ids = await startRadio(trackId);
      if (ids.length === 0) return;
      await playerPlayTracks("radio", null, ids, 0);
    } catch (err) {
      console.error("[trackContextMenu] start radio failed", err);
    }
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

  const handleSetRating = useCallback(
    async (trackId: number, popm: number | null) => {
      try {
        await setTrackRating(trackId, popm);
        // Backend emits `track:updated` — views listening via
        // useTrackUpdated refetch on their own. No local state to
        // touch here.
      } catch (err) {
        console.error("[trackContextMenu] set rating failed", err);
      }
    },
    [],
  );

  const handleShowProperties = useCallback((track: Track) => {
    setPropertiesTrack(track);
  }, []);

  const closeProperties = useCallback(() => setPropertiesTrack(null), []);

  const handleShowBatchEdit = useCallback((ids: number[]) => {
    setBatchEditIds(ids);
  }, []);

  const closeBatchEdit = useCallback(() => setBatchEditIds(null), []);

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
            onStartRadio={handleStartRadio}
            onAddToPlaylist={handleAddToPlaylist}
            onCreatePlaylist={onCreatePlaylist}
            onToggleLike={handleToggleLike}
            onSetRating={handleSetRating}
            onRemoveFromPlaylist={onRemoveFromPlaylist}
            onNavigateToAlbum={onNavigateToAlbum}
            onNavigateToArtist={onNavigateToArtist}
            onShowProperties={handleShowProperties}
            batchEditIds={selectedTrackIds}
            onShowBatchEdit={handleShowBatchEdit}
          />
        )}
        <TrackPropertiesModal
          key={propertiesTrack?.id ?? "none"}
          track={propertiesTrack}
          onClose={closeProperties}
        />
        <BatchTagEditModal
          trackIds={batchEditIds}
          onClose={closeBatchEdit}
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
    handleStartRadio,
    handleAddToPlaylist,
    onCreatePlaylist,
    handleToggleLike,
    handleSetRating,
    onRemoveFromPlaylist,
    onNavigateToAlbum,
    onNavigateToArtist,
    handleShowProperties,
    propertiesTrack,
    closeProperties,
    selectedTrackIds,
    handleShowBatchEdit,
    batchEditIds,
    closeBatchEdit,
  ]);

  return { open, close, render };
}

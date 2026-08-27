import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Play,
  Shuffle,
  Clock,
  Music2,
  Heart,
  ImageIcon,
  Film,
} from "lucide-react";
import {
  remoteGetAlbum,
  remotePlayTracks,
  type RemoteAlbum,
} from "../../lib/tauri/remoteServer";
import { RemoteArtwork } from "../common/RemoteArtwork";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { EmptyState } from "../common/EmptyState";
import { DetailViewSkeleton } from "../common/DetailViewSkeleton";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { CoverPickerModal } from "../common/CoverPickerModal";
import { MotionCoverPickerModal } from "../common/MotionCoverPickerModal";
import { HiResBadge } from "../common/HiResBadge";
import { PlayingIndicator } from "../common/PlayingIndicator";
import { SelectionActionBar } from "../common/SelectionActionBar";
import { Lightbox } from "../common/Lightbox";
import { convertFileSrc } from "@tauri-apps/api/core";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import { useTrackContextMenu } from "../../hooks/useTrackContextMenu";
import { useTrackUpdated } from "../../hooks/useTrackUpdated";
import { useMultiSelect } from "../../hooks/useMultiSelect";
import {
  getAlbumDetail,
  enrichAlbumDeezer,
  type AlbumDetail,
  type AlbumTrack,
} from "../../lib/tauri/detail";
import {
  formatDuration,
  listLikedTrackIds,
  toggleLikeTrack,
  type Track,
} from "../../lib/tauri/track";

/**
 * A server album in the shape the view already speaks.
 *
 * Mapping rather than branching everywhere: the header, the meta line and the
 * track table all read `AlbumDetail`, and a second shape would mean a second
 * version of each. What the server has no answer for is `null` — a label, a
 * release date, genres — which is what those fields already mean locally when
 * enrichment has not run.
 */
function toAlbumDetail(album: RemoteAlbum): AlbumDetail {
  return {
    // Never read: the view checks `remote` before anything that needs a rowid.
    id: -1,
    title: album.title,
    artist_id: null,
    artist_name: album.artist,
    year: album.year,
    track_count: album.tracks.length,
    total_duration_ms: album.tracks.reduce(
      (sum, track) => sum + (track.duration_ms ?? 0),
      0,
    ),
    artwork_path: null,
    artwork_path_1x: null,
    artwork_path_2x: null,
    // The album's own cover. Derived from the first track that had one until
    // now, which is wrong twice: an album with no tracks showed none, and an
    // album whose cover differs from its first track's showed the track's.
    artwork_hash: album.artwork_hash,
    label: null,
    release_date: null,
    genres: [],
    tracks: album.tracks.map((track, index) => ({
      source: "remote" as const,
      remote_id: track.id,
      artwork_hash: track.artwork_hash,
      // Negative so a leak is obvious rather than colliding with a real rowid.
      id: -(index + 1),
      title: track.title ?? "",
      artist_id: null,
      artist_name: track.artist,
      artist_ids: null,
      duration_ms: track.duration_ms ?? 0,
      track_number: index + 1,
      disc_number: 1,
      artwork_path: null,
      artwork_path_1x: null,
      artwork_path_2x: null,
      file_path: "",
      bit_depth: null,
      sample_rate: null,
      codec: null,
      year: album.year,
      bitrate: null,
      channels: null,
      musical_key: null,
      file_size: 0,
      added_at: 0,
      rating: null,
    })),
  };
}

interface AlbumDetailViewProps {
  albumId: number | null;
  /** Set instead of `albumId` when the album is the bound server's. Exactly
   *  one of the two is ever set: they are two catalogues, not two ids for one
   *  album. */
  remoteAlbumId?: string | null;
  onNavigateToArtist: (artistId: number) => void;
  onNavigateToRemoteArtist?: (remoteArtistId: string) => void;
}

/**
 * One album's detail, from the device or from the bound server.
 *
 * The server's albums had a view of their own until now, and it was poorer by
 * construction: no ratings, no motion cover, no selection, no context menu —
 * not because a server album cannot have them but because every feature landed
 * on the local side and the twin was never updated. One view stops that: what
 * a server album cannot have is now absent for a stated reason rather than by
 * omission.
 */
export function AlbumDetailView({
  albumId,
  remoteAlbumId = null,
  onNavigateToArtist,
  onNavigateToRemoteArtist,
}: AlbumDetailViewProps) {
  const { t } = useTranslation();
  // Which catalogue this album came from. Everything that touches a local
  // rowid, a file or the local user data is gated on it.
  const remote = remoteAlbumId != null;
  const { playTracks, currentTrack, toggleShuffle, isShuffled, isPlaying } =
    usePlayer();
  const { createPlaylist } = usePlaylist();

  const [album, setAlbum] = useState<AlbumDetail | null>(null);
  // The server's artist identifier, kept beside the mapped album: it is a
  // string and `AlbumDetail.artist_id` is a rowid, so it has nowhere to go in
  // the shared shape.
  const [remoteArtistId, setRemoteArtistId] = useState<string | null>(null);
  /**
   * Which album the loaded one *is*.
   *
   * `remote` is derived from the props and flips the instant navigation
   * happens, while `album` still holds the album that was showing. In that
   * window the two disagree, and the disagreement is not cosmetic: playing
   * would take the remote branch over a local album's tracks and hand the
   * server a list of empty identifiers, and the artist link would go to the
   * previous album's artist. A snapshot of another album counts as absent,
   * not as approximate — the same rule the audio pipeline popover needed.
   *
   * Stamped rather than cleared, so a cover change or a tag edit refetches
   * without flashing a skeleton: those do not change the identity.
   */
  const [loadedKey, setLoadedKey] = useState<string | null>(null);
  // Init true so the skeleton paints on first render — paired with the
  // `!album && !isLoading` early-return below, this also prevents a
  // one-frame "album not found" flash before the fetch schedules.
  const [isLoading, setIsLoading] = useState(true);
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());
  const [isCreatePlaylistModalOpen, setIsCreatePlaylistModalOpen] =
    useState(false);
  const [isCoverPickerOpen, setIsCoverPickerOpen] = useState(false);
  const [isMotionCoverPickerOpen, setIsMotionCoverPickerOpen] = useState(false);
  const [coverReloadKey, setCoverReloadKey] = useState(0);
  const [isLightboxOpen, setIsLightboxOpen] = useState(false);
  const selection = useMultiSelect<Track>();

  const trackContextMenu = useTrackContextMenu({
    likedIds,
    onLikedChanged: (trackId, nowLiked) =>
      setLikedIds((prev) => {
        const next = new Set(prev);
        if (nowLiked) next.add(trackId);
        else next.delete(trackId);
        return next;
      }),
    onCreatePlaylist: () => setIsCreatePlaylistModalOpen(true),
    // No `onNavigateToAlbum` — we're already on the album page.
    onNavigateToArtist,
    selectedTrackIds: [...selection.selectedIds],
  });

  // Deezer enrichment overlay
  const [enrichedLabel, setEnrichedLabel] = useState<string | null>(null);
  const [enrichedDate, setEnrichedDate] = useState<string | null>(null);

  // Refetch on tag-edit so the row updates without re-navigation.
  const [editRefetch, setEditRefetch] = useState(0);
  useTrackUpdated(useCallback(() => setEditRefetch((k) => k + 1), []));

  // The identity of what is being asked for, as opposed to what is loaded.
  const albumKey =
    remoteAlbumId != null ? `remote:${remoteAlbumId}` : `local:${albumId}`;

  // Load album detail
  useEffect(() => {
    if (albumId == null && remoteAlbumId == null) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setAlbum(null);
      return;
    }
    let cancelled = false;
    (async () => {
      setIsLoading(true);
      setEnrichedLabel(null);
      setEnrichedDate(null);
      try {
        if (remoteAlbumId != null) {
          const fetched = await remoteGetAlbum(remoteAlbumId);
          if (!cancelled) {
            setAlbum(toAlbumDetail(fetched));
            setRemoteArtistId(fetched.artist_id);
            setLoadedKey(albumKey);
          }
        } else {
          const detail = await getAlbumDetail(albumId as number);
          if (!cancelled) {
            setAlbum(detail);
            setRemoteArtistId(null);
            setLoadedKey(albumKey);
          }
        }
      } catch (err) {
        console.error("[AlbumDetailView] load failed", err);
        if (!cancelled) setAlbum(null);
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [albumId, remoteAlbumId, albumKey, coverReloadKey, editRefetch]);

  // Load liked IDs
  useEffect(() => {
    listLikedTrackIds()
      .then((ids) => setLikedIds(new Set(ids)))
      .catch(() => {});
  }, [albumId]);

  // Clear selection when switching albums.
  const clearSelection = selection.clear;
  useEffect(() => {
    clearSelection();
  }, [albumId, remoteAlbumId, clearSelection]);

  // Deezer enrichment (async, fire-and-forget)
  useEffect(() => {
    if (albumId == null) return;
    let cancelled = false;
    enrichAlbumDeezer(albumId)
      .then((e) => {
        if (cancelled) return;
        if (e.label) setEnrichedLabel(e.label);
        if (e.release_date) setEnrichedDate(e.release_date);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [albumId]);

  const handleToggleLike = async (trackId: number) => {
    const nowLiked = await toggleLikeTrack(trackId);
    setLikedIds((prev) => {
      const next = new Set(prev);
      if (nowLiked) next.add(trackId);
      else next.delete(trackId);
      return next;
    });
  };

  // Either identifier will do. Testing only the local one sent every server
  // album straight to the empty state — the fetch above ran, the mapping ran,
  // and nothing was ever shown.
  if ((albumId == null && remoteAlbumId == null) || (!album && !isLoading)) {
    return (
      <EmptyState
        icon={<Music2 size={40} />}
        title={t("albumDetail.emptyTitle")}
        description={t("albumDetail.emptyDescription")}
        className="py-20"
      />
    );
  }

  // Loading, or holding another album's snapshot — which is the same thing as
  // far as everything below is concerned.
  if (!album || loadedKey !== albumKey) {
    return <DetailViewSkeleton ariaLabel={t("albumDetail.badge")} />;
  }

  // Build playable Track[] from AlbumTrack[] for the player AND for the
  // track context menu — which is what feeds the Properties modal. Every
  // field the modal reads must come from the row rather than a
  // placeholder: hard-coded nulls here are what left the Audio and File
  // sections blank on this view alone (issue #458).
  //
  // That includes `rating`: the context menu's rating submenu is on by
  // default here, so the previous placeholder made an already-rated
  // track show up as unrated — the exact misreport `enableRating:
  // false` exists to prevent on surfaces that genuinely lack it.
  const playableTracks = album.tracks.map((at) => ({
    id: at.id,
    library_id: 0,
    title: at.title,
    album_id: album.id,
    album_title: album.title,
    artist_id: at.artist_id,
    artist_name: at.artist_name,
    artist_ids: at.artist_ids,
    duration_ms: at.duration_ms,
    track_number: at.track_number,
    disc_number: at.disc_number,
    // The track's own year, falling back to the album's — a
    // compilation can carry a per-track year the album header doesn't.
    year: at.year ?? album.year,
    bitrate: at.bitrate,
    sample_rate: at.sample_rate,
    channels: at.channels,
    bit_depth: at.bit_depth,
    codec: at.codec,
    musical_key: at.musical_key,
    file_path: at.file_path,
    file_size: at.file_size,
    added_at: at.added_at,
    artwork_path: at.artwork_path,
    artwork_path_1x: at.artwork_path_1x,
    artwork_path_2x: at.artwork_path_2x,
    rating: at.rating,
  }));

  // The two engines keep separate queues (RFC-005 decision 9), so a server
  // album plays through the remote one. Every track here shares a source, so
  // unlike the mixed library list there is no run to pick out.
  // A plain function, not a hook: it lives past the early returns above, and
  // `album` is non-null by here.
  const playFrom = async (index: number) => {
    if (remote) {
      const ids = album.tracks.map((track) => track.remote_id ?? "");
      if (ids.length === 0) return;
      await remotePlayTracks(ids, index);
      return;
    }
    if (playableTracks.length === 0) return;
    await playTracks(playableTracks, index, { type: "library", id: null });
  };

  const handlePlayAll = async () => {
    await playFrom(0);
  };

  const handleShufflePlay = async () => {
    if (remote) {
      // Shuffle is a local-queue mode; the remote queue has none, so the
      // button is not offered and this is unreachable.
      return;
    }
    if (playableTracks.length === 0) return;
    await playTracks(playableTracks, 0, { type: "library", id: null });
    // Gate the toggle so we never *disable* shuffle when the user
    // explicitly clicks the Shuffle button.
    if (!isShuffled) await toggleShuffle();
  };

  const label = enrichedLabel ?? album.label;
  const releaseDate = enrichedDate ?? album.release_date;

  // Check if multi-disc
  const discNumbers = [
    ...new Set(album.tracks.map((t) => t.disc_number ?? 1)),
  ].sort((a, b) => a - b);
  const isMultiDisc = discNumbers.length > 1;

  return (
    <div className="space-y-6 animate-fade-in pb-12">
      {/* Header. Sized for 1080p / 125 % DPI: cover at 11rem (176 px)
          keeps the album visually anchored without dominating the
          viewport, title wraps to a second line instead of truncating
          when the side lyrics panel is open. */}
      <div className="flex items-start space-x-6">
        {/* Album artwork. A server cover is resolved by hash through the cover
            cache and has no lightbox: the lightbox opens the original file,
            and there is no file here. */}
        {remote ? (
          <RemoteArtwork
            hash={album.artwork_hash ?? null}
            className="w-44 h-44 rounded-2xl shadow-lg shrink-0"
            iconSize={64}
          />
        ) : album.artwork_path ? (
          <button
            type="button"
            onClick={() => setIsLightboxOpen(true)}
            aria-label={t("common.viewArtwork")}
            className="cursor-zoom-in focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 rounded-2xl shrink-0"
          >
            <Artwork
              path={album.artwork_path}
              path1x={album.artwork_path_1x}
              path2x={album.artwork_path_2x}
              size="full"
              className="w-44 h-44 shadow-lg"
              iconSize={64}
              alt={album.title}
              rounded="2xl"
            />
          </button>
        ) : (
          <Artwork
            path={album.artwork_path}
            path1x={album.artwork_path_1x}
            path2x={album.artwork_path_2x}
            size="full"
            className="w-44 h-44 shadow-lg shrink-0"
            iconSize={64}
            alt={album.title}
            rounded="2xl"
          />
        )}

        <div className="flex-1 min-w-0 pt-1">
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
            {t("albumDetail.badge")}
          </div>
          <h1 className="text-3xl md:text-4xl font-bold mb-1 text-zinc-900 dark:text-white line-clamp-2">
            {album.title}
          </h1>

          {/* Artist (clickable) */}
          {album.artist_name && (
            <button
              type="button"
              onClick={() => {
                if (remote) {
                  if (remoteArtistId && onNavigateToRemoteArtist) {
                    onNavigateToRemoteArtist(remoteArtistId);
                  }
                  return;
                }
                if (album.artist_id != null) onNavigateToArtist(album.artist_id);
              }}
              className="text-lg font-medium text-emerald-600 dark:text-emerald-400 hover:underline mb-2"
            >
              {album.artist_name}
            </button>
          )}

          {/* Meta */}
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-zinc-500 mb-3">
            {album.year && <span>{album.year}</span>}
            {album.year && label && <span>·</span>}
            {label && <span>{label}</span>}
            {(album.year || label) && <span>·</span>}
            <span>
              {t("albumDetail.trackCount", { count: album.track_count })}
            </span>
            <span>·</span>
            <span>{formatDuration(album.total_duration_ms)}</span>
          </div>

          {/* Genres */}
          {album.genres.length > 0 && (
            <div className="flex flex-wrap gap-2 mb-3">
              {album.genres.map((genre) => (
                <span
                  key={genre}
                  className="text-xs px-2.5 py-1 rounded-full bg-zinc-100 text-zinc-600 dark:bg-zinc-800 dark:text-zinc-400"
                >
                  {genre}
                </span>
              ))}
            </div>
          )}

          {/* Release date (from Deezer) */}
          {releaseDate && (
            <div className="text-xs text-zinc-400">
              {t("albumDetail.releaseDate")}: {releaseDate}
            </div>
          )}

          {/* Actions */}
          <div className="flex flex-wrap items-center gap-2 mt-3">
            <button
              type="button"
              onClick={handlePlayAll}
              disabled={album.tracks.length === 0}
              className="bg-emerald-500 hover:bg-emerald-600 text-white px-4 py-2 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm disabled:opacity-50"
            >
              <Play size={16} className="fill-current" />
              <span>{t("albumDetail.playAll")}</span>
            </button>
            {/* Shuffle is a mode of the local queue; the remote one has none. */}
            {!remote && (
            <button
              type="button"
              onClick={handleShufflePlay}
              disabled={album.tracks.length === 0}
              className="border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 hover:bg-zinc-50 dark:hover:bg-zinc-700 text-zinc-700 dark:text-zinc-300 px-4 py-2 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm disabled:opacity-50"
            >
              <Shuffle size={16} />
              <span>{t("albumDetail.shuffle")}</span>
            </button>
            )}
            {/* Both covers are written into the local library — there is no
                local album row here to write one to. */}
            {!remote && (
            <>
            <button
              type="button"
              onClick={() => setIsCoverPickerOpen(true)}
              className="border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 hover:bg-zinc-50 dark:hover:bg-zinc-700 text-zinc-700 dark:text-zinc-300 px-4 py-2 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm"
            >
              <ImageIcon size={16} />
              <span>{t("library.changeCover")}</span>
            </button>
            <button
              type="button"
              onClick={() => setIsMotionCoverPickerOpen(true)}
              className="border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 hover:bg-zinc-50 dark:hover:bg-zinc-700 text-zinc-700 dark:text-zinc-300 px-4 py-2 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm"
            >
              <Film size={16} />
              <span>{t("albumDetail.setMotionCover")}</span>
            </button>
            </>
            )}
          </div>
        </div>
      </div>

      {/* Track list */}
      {album.tracks.length > 0 ? (
        <AlbumTrackTable
          tracks={album.tracks}
          playableTracks={playableTracks}
          isLoading={isLoading}
          isMultiDisc={isMultiDisc}
          discNumbers={discNumbers}
          currentTrackId={currentTrack?.id ?? null}
          isPlaying={isPlaying}
          likedIds={likedIds}
          onToggleLike={handleToggleLike}
          onPlayTrack={(index) => void playFrom(index)}
          onNavigateToArtist={onNavigateToArtist}
          onContextMenuRow={trackContextMenu.open}
          onRowMenuKey={trackContextMenu.openFromKeyboard}
          t={t}
          isSelected={selection.isSelected}
          onRowSelect={(track, e) => {
            if (e.shiftKey) {
              selection.selectRange(track.id, playableTracks);
            } else if (e.ctrlKey || e.metaKey) {
              selection.toggleOne(track.id);
            } else {
              selection.setSingle(track.id);
            }
          }}
        />
      ) : (
        <EmptyState
          icon={<Music2 size={40} />}
          title={t("albumDetail.emptyTracksTitle")}
          description={t("albumDetail.emptyTracksDescription")}
          className="py-20"
        />
      )}

      <CreatePlaylistModal
        isOpen={isCreatePlaylistModalOpen}
        onClose={() => setIsCreatePlaylistModalOpen(false)}
        onCreate={async (data) => {
          try {
            await createPlaylist({
              name: data.name,
              description: data.description || null,
              color_id: data.colorId,
              icon_id: data.iconId,
            });
          } catch (err) {
            console.error("[AlbumDetailView] create playlist failed", err);
          }
        }}
      />

      {/* Mounted only for a local album: their buttons are hidden for a
          server one, but mounting them anyway would hand each a rowid that
          is the negative sentinel. A component that never opens should not
          be holding an invalid identifier in the meantime. */}
      {!remote && (
      <>
      <CoverPickerModal
        albumId={album.id}
        initialQuery={
          album.artist_name
            ? `${album.title} ${album.artist_name}`
            : album.title
        }
        isOpen={isCoverPickerOpen}
        onClose={() => setIsCoverPickerOpen(false)}
        onSuccess={() => setCoverReloadKey((k) => k + 1)}
      />

      <MotionCoverPickerModal
        albumId={album.id}
        isOpen={isMotionCoverPickerOpen}
        onClose={() => setIsMotionCoverPickerOpen(false)}
        onSuccess={() => setCoverReloadKey((k) => k + 1)}
      />
      </>
      )}

      {trackContextMenu.render()}

      <Lightbox
        src={album.artwork_path ? convertFileSrc(album.artwork_path) : null}
        alt={album.title}
        isOpen={isLightboxOpen}
        onClose={() => setIsLightboxOpen(false)}
      />

      {albumId != null && (
        <SelectionActionBar
          trackIds={[...selection.selectedIds]}
          context={{ type: "album", albumId }}
          onClear={selection.clear}
          onCreatePlaylist={() => setIsCreatePlaylistModalOpen(true)}
        />
      )}
    </div>
  );
}

// ── Track table ─────────────────────────────────────────────────────

interface AlbumTrackTableProps {
  tracks: AlbumTrack[];
  playableTracks: Track[];
  isLoading: boolean;
  isMultiDisc: boolean;
  discNumbers: number[];
  currentTrackId: number | null;
  isPlaying: boolean;
  likedIds: Set<number>;
  onToggleLike: (trackId: number) => void;
  onPlayTrack: (index: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  onContextMenuRow: (event: React.MouseEvent, track: Track) => void;
  /** Keyboard equivalent (Menu / Shift+F10). Returns `true` when it
   *  opened the menu, so the row's own key handling can stand down. */
  onRowMenuKey: (event: React.KeyboardEvent, track: Track) => boolean;
  t: (key: string, opts?: Record<string, unknown>) => string;
  isSelected: (id: number) => boolean;
  onRowSelect: (track: Track, e: React.MouseEvent) => void;
}

function AlbumTrackTable({
  tracks,
  playableTracks,
  isLoading,
  isMultiDisc,
  discNumbers,
  currentTrackId,
  isPlaying,
  likedIds,
  onToggleLike,
  onPlayTrack,
  onNavigateToArtist,
  onContextMenuRow,
  onRowMenuKey,
  t,
  isSelected,
  onRowSelect,
}: AlbumTrackTableProps) {
  const gridCols = "grid-cols-[3rem_1fr_1fr_5rem_2rem]";

  const renderTrackRow = (track: AlbumTrack, globalIndex: number) => {
    // A server track has no local rowid: the one on the row is a negative
    // sentinel. Everything keyed on a rowid — the current-track highlight,
    // the selection, the like list, the context menu — is gated here rather
    // than reading that sentinel and quietly matching nothing.
    const local = track.source !== "remote";
    const playable = local ? playableTracks[globalIndex] : null;
    const isCurrent = local && track.id === currentTrackId;
    const isRowSelected = local && isSelected(track.id);
    return (
      // Row can't be a <button> because it contains action buttons;
      // nested buttons are invalid HTML. Keyboard activation still
      // works via tabIndex + onKeyDown.
      <li
        key={track.remote_id ?? `${track.id}-${globalIndex}`}
        tabIndex={0}
        role="button"
        onClick={(e) => {
          if (playable) onRowSelect(playable, e);
        }}
        onDoubleClick={() => onPlayTrack(globalIndex)}
        onKeyDown={(e) => {
          // Only play when the row itself is focused — see LibraryView.
          if (e.target !== e.currentTarget) return;
          if (playable && onRowMenuKey(e, playable)) return;
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onPlayTrack(globalIndex);
          }
        }}
        onKeyUp={(e) => {
          if (e.target !== e.currentTarget) return;
          if (e.key === " ") e.preventDefault();
        }}
        onContextMenu={(e) => {
          if (playable) onContextMenuRow(e, playable);
        }}
        className={`grid ${gridCols} gap-4 px-5 py-2 items-center select-none transition-colors cursor-pointer focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-emerald-500 ${
          isRowSelected
            ? "bg-blue-500/15 ring-1 ring-inset ring-blue-500/40 dark:bg-blue-500/20"
            : isCurrent
              ? "bg-emerald-50 dark:bg-emerald-900/20"
              : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
        }`}
      >
        <span
          className={`text-right text-sm tabular-nums flex items-center justify-end ${
            isCurrent ? "text-emerald-500 font-semibold" : "text-zinc-400"
          }`}
        >
          {isCurrent ? (
            <PlayingIndicator isPlaying={isPlaying} />
          ) : (
            (track.track_number ?? globalIndex + 1)
          )}
        </span>
        <div className="min-w-0">
          <span
            className={`text-sm truncate flex items-center gap-2 ${
              isCurrent
                ? "text-emerald-600 dark:text-emerald-400 font-semibold"
                : "text-zinc-800 dark:text-zinc-200"
            }`}
          >
            <span className="truncate">{track.title}</span>
            <HiResBadge
              bitDepth={track.bit_depth}
              sampleRate={track.sample_rate}
              codec={track.codec}
              variant="inline"
            />
          </span>
          {track.artist_name && (
            <span className="text-xs text-zinc-500 truncate block">
              {track.artist_name}
            </span>
          )}
        </div>
        <ArtistLink
          name={track.artist_name}
          artistIds={track.artist_ids}
          onNavigate={onNavigateToArtist}
          // The album's own artist link already goes to the right place; a
          // per-row one would need each track's server artist id, which the
          // album payload does not carry.
          fallback={t("library.table.unknown")}
          className="text-sm text-zinc-500 truncate"
        />
        <span className="text-sm tabular-nums text-zinc-400 text-right">
          {formatDuration(track.duration_ms)}
        </span>
        <div className="flex justify-center">
          {/* The like list keys on a local rowid a server track does not
              have. Absent rather than inert: an empty heart that does
              nothing reads as "not liked". */}
          {local && (
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                onToggleLike(track.id);
              }}
              aria-label={
                likedIds.has(track.id) ? t("liked.unlike") : t("liked.like")
              }
              className={`p-1 rounded-full transition-colors ${
                likedIds.has(track.id)
                  ? "text-pink-500"
                  : "text-zinc-300 dark:text-zinc-600 hover:text-pink-500"
              }`}
            >
              <Heart
                size={14}
                className={likedIds.has(track.id) ? "fill-current" : ""}
              />
            </button>
          )}
        </div>
      </li>
    );
  };

  return (
    <div className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 overflow-hidden">
      {/* Header */}
      <div
        className={`grid ${gridCols} gap-4 px-5 py-3 text-[10px] font-bold tracking-widest text-zinc-400 uppercase border-b border-zinc-100 dark:border-zinc-800`}
      >
        <span className="text-right">{t("library.table.number")}</span>
        <span>{t("library.table.title")}</span>
        <span>{t("library.table.artist")}</span>
        <span
          className="flex justify-end"
          aria-label={t("library.table.duration")}
        >
          <Clock size={14} />
        </span>
        <span aria-hidden="true" />
      </div>

      <ul
        className={`divide-y divide-zinc-100 dark:divide-zinc-800/60 ${
          isLoading ? "opacity-50" : ""
        }`}
      >
        {isMultiDisc
          ? discNumbers.map((discNum) => {
              const discTracks = tracks.filter(
                (t) => (t.disc_number ?? 1) === discNum,
              );
              return (
                <li key={`disc-${discNum}`}>
                  <div className="px-5 py-2 bg-zinc-50 dark:bg-zinc-800/30 text-xs font-bold tracking-widest text-zinc-400 uppercase">
                    {t("albumDetail.discHeader", { number: discNum })}
                  </div>
                  <ul className="divide-y divide-zinc-100 dark:divide-zinc-800/60">
                    {discTracks.map((track) => {
                      const globalIndex = tracks.indexOf(track);
                      return renderTrackRow(track, globalIndex);
                    })}
                  </ul>
                </li>
              );
            })
          : tracks.map((track, index) => renderTrackRow(track, index))}
      </ul>
    </div>
  );
}

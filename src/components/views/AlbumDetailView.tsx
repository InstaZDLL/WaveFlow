import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Play, Shuffle, Clock, Music2, Heart, ImageIcon } from "lucide-react";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { EmptyState } from "../common/EmptyState";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { CoverPickerModal } from "../common/CoverPickerModal";
import { HiResBadge } from "../common/HiResBadge";
import { SelectionActionBar } from "../common/SelectionActionBar";
import { Lightbox } from "../common/Lightbox";
import { convertFileSrc } from "@tauri-apps/api/core";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import { useTrackContextMenu } from "../../hooks/useTrackContextMenu";
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

interface AlbumDetailViewProps {
  albumId: number | null;
  onNavigateToArtist: (artistId: number) => void;
}

export function AlbumDetailView({
  albumId,
  onNavigateToArtist,
}: AlbumDetailViewProps) {
  const { t } = useTranslation();
  const { playTracks, currentTrack, toggleShuffle } = usePlayer();
  const { createPlaylist } = usePlaylist();

  const [album, setAlbum] = useState<AlbumDetail | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());
  const [isCreatePlaylistModalOpen, setIsCreatePlaylistModalOpen] = useState(false);
  const [isCoverPickerOpen, setIsCoverPickerOpen] = useState(false);
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
  });

  // Deezer enrichment overlay
  const [enrichedLabel, setEnrichedLabel] = useState<string | null>(null);
  const [enrichedDate, setEnrichedDate] = useState<string | null>(null);

  // Load album detail
  useEffect(() => {
    if (albumId == null) {
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
        const detail = await getAlbumDetail(albumId);
        if (!cancelled) setAlbum(detail);
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
  }, [albumId, coverReloadKey]);

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
  }, [albumId, clearSelection]);

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

  if (albumId == null || (!album && !isLoading)) {
    return (
      <EmptyState
        icon={<Music2 size={40} />}
        title={t("albumDetail.emptyTitle")}
        description={t("albumDetail.emptyDescription")}
        className="py-20"
      />
    );
  }

  if (!album) return null; // loading

  // Build playable Track[] from AlbumTrack[] for the player
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
    year: album.year,
    bitrate: null,
    sample_rate: at.sample_rate,
    channels: null,
    bit_depth: at.bit_depth,
    codec: null,
    file_path: at.file_path,
    file_size: 0,
    added_at: 0,
    artwork_path: at.artwork_path,
    artwork_path_1x: at.artwork_path_1x,
    artwork_path_2x: at.artwork_path_2x,
    rating: null,
  }));

  const handlePlayAll = async () => {
    if (playableTracks.length === 0) return;
    await playTracks(playableTracks, 0, { type: "library", id: null });
  };

  const handleShufflePlay = async () => {
    if (playableTracks.length === 0) return;
    await playTracks(playableTracks, 0, { type: "library", id: null });
    await toggleShuffle();
  };

  const label = enrichedLabel ?? album.label;
  const releaseDate = enrichedDate ?? album.release_date;

  // Check if multi-disc
  const discNumbers = [
    ...new Set(album.tracks.map((t) => t.disc_number ?? 1)),
  ].sort((a, b) => a - b);
  const isMultiDisc = discNumbers.length > 1;

  return (
    <div className="max-w-6xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Header */}
      <div className="flex items-start space-x-8">
        <div
          onDoubleClick={() => {
            if (album.artwork_path) setIsLightboxOpen(true);
          }}
          className={album.artwork_path ? "cursor-zoom-in" : undefined}
        >
          <Artwork
            path={album.artwork_path}
            path1x={album.artwork_path_1x}
            path2x={album.artwork_path_2x}
            size="full"
            className="w-48 h-48 shadow-lg"
            iconSize={64}
            alt={album.title}
            rounded="2xl"
          />
        </div>

        <div className="flex-1 min-w-0 pt-2">
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
            {t("albumDetail.badge")}
          </div>
          <h1 className="text-4xl font-bold mb-2 text-zinc-900 dark:text-white truncate">
            {album.title}
          </h1>

          {/* Artist (clickable) */}
          {album.artist_name && (
            <button
              type="button"
              onClick={() =>
                album.artist_id != null &&
                onNavigateToArtist(album.artist_id)
              }
              className="text-lg font-medium text-emerald-600 dark:text-emerald-400 hover:underline mb-3"
            >
              {album.artist_name}
            </button>
          )}

          {/* Meta */}
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-zinc-500 mb-4">
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
            <div className="flex flex-wrap gap-2 mb-4">
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
          <div className="flex items-center space-x-3 mt-4">
            <button
              type="button"
              onClick={handlePlayAll}
              disabled={album.tracks.length === 0}
              className="bg-emerald-500 hover:bg-emerald-600 text-white px-5 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm disabled:opacity-50"
            >
              <Play size={16} className="fill-current" />
              <span>{t("albumDetail.playAll")}</span>
            </button>
            <button
              type="button"
              onClick={handleShufflePlay}
              disabled={album.tracks.length === 0}
              className="border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 hover:bg-zinc-50 dark:hover:bg-zinc-700 text-zinc-700 dark:text-zinc-300 px-5 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm disabled:opacity-50"
            >
              <Shuffle size={16} />
              <span>{t("albumDetail.shuffle")}</span>
            </button>
            <button
              type="button"
              onClick={() => setIsCoverPickerOpen(true)}
              className="border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 hover:bg-zinc-50 dark:hover:bg-zinc-700 text-zinc-700 dark:text-zinc-300 px-5 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm"
            >
              <ImageIcon size={16} />
              <span>{t("library.changeCover")}</span>
            </button>
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
          likedIds={likedIds}
          onToggleLike={handleToggleLike}
          onPlayTrack={(index) =>
            playTracks(playableTracks, index, {
              type: "library",
              id: null,
            })
          }
          onNavigateToArtist={onNavigateToArtist}
          onContextMenuRow={trackContextMenu.open}
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
  likedIds: Set<number>;
  onToggleLike: (trackId: number) => void;
  onPlayTrack: (index: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  onContextMenuRow: (event: React.MouseEvent, track: Track) => void;
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
  likedIds,
  onToggleLike,
  onPlayTrack,
  onNavigateToArtist,
  onContextMenuRow,
  t,
  isSelected,
  onRowSelect,
}: AlbumTrackTableProps) {
  const gridCols = "grid-cols-[3rem_1fr_1fr_5rem_2rem]";

  const renderTrackRow = (track: AlbumTrack, globalIndex: number) => {
    const isCurrent = track.id === currentTrackId;
    const isRowSelected = isSelected(track.id);
    return (
      <li
        key={`${track.id}-${globalIndex}`}
        onClick={(e) => onRowSelect(playableTracks[globalIndex], e)}
        onDoubleClick={() => onPlayTrack(globalIndex)}
        onContextMenu={(e) => onContextMenuRow(e, playableTracks[globalIndex])}
        className={`grid ${gridCols} gap-4 px-5 py-2 items-center select-none transition-colors cursor-pointer ${
          isRowSelected
            ? "bg-blue-500/15 ring-1 ring-inset ring-blue-500/40 dark:bg-blue-500/20"
            : isCurrent
            ? "bg-emerald-50 dark:bg-emerald-900/20"
            : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
        }`}
      >
        <span
          className={`text-right text-sm tabular-nums ${
            isCurrent
              ? "text-emerald-500 font-semibold"
              : "text-zinc-400"
          }`}
        >
          {track.track_number ?? globalIndex + 1}
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
          fallback={t("library.table.unknown")}
          className="text-sm text-zinc-500 truncate"
        />
        <span className="text-sm tabular-nums text-zinc-400 text-right">
          {formatDuration(track.duration_ms)}
        </span>
        <div className="flex justify-center">
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
        <span className="flex justify-end" aria-label={t("library.table.duration")}>
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

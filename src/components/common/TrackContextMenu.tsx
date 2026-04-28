import { useTranslation } from "react-i18next";
import {
  Disc3,
  ExternalLink,
  Heart,
  Info,
  ListEnd,
  ListPlus,
  Plus,
  Trash2,
  User,
} from "lucide-react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  ContextMenu,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuSub,
  type ContextMenuPoint,
} from "./ContextMenu";
import type { Track } from "../../lib/tauri/track";
import type { Playlist } from "../../lib/tauri/playlist";
import { resolvePlaylistColor } from "../../lib/playlistVisuals";
import { PlaylistIcon } from "../../lib/PlaylistIcon";

export interface TrackContextMenuProps {
  point: ContextMenuPoint;
  track: Track;
  playlists: Playlist[];
  isLiked: boolean;
  /** Set when the menu is opened from inside a playlist view; enables
   *  the "Remove from this playlist" item. */
  currentPlaylistId?: number | null;
  onClose: () => void;
  onPlayNext: (trackId: number) => void;
  onAddToQueue: (trackId: number) => void;
  onAddToPlaylist: (playlistId: number, trackId: number) => void;
  onCreatePlaylist: () => void;
  onToggleLike: (trackId: number) => void;
  onRemoveFromPlaylist?: (playlistId: number, trackId: number) => void;
  onNavigateToAlbum?: (albumId: number) => void;
  onNavigateToArtist?: (artistId: number) => void;
  onShowProperties: (track: Track) => void;
}

interface ArtistEntry {
  id: number;
  name: string;
}

/**
 * Spotify-style right-click menu for a track row. Composes the generic
 * ContextMenu primitive with the track-specific actions: queue ops,
 * playlist add (submenu), like toggle, navigation to album/artist
 * (submenu when multi-artist), reveal-in-explorer, and remove-from-playlist
 * when relevant.
 */
export function TrackContextMenu({
  point,
  track,
  playlists,
  isLiked,
  currentPlaylistId,
  onClose,
  onPlayNext,
  onAddToQueue,
  onAddToPlaylist,
  onCreatePlaylist,
  onToggleLike,
  onRemoveFromPlaylist,
  onNavigateToAlbum,
  onNavigateToArtist,
  onShowProperties,
}: TrackContextMenuProps) {
  const { t } = useTranslation();
  const artists = parseArtistList(track.artist_ids, track.artist_name);

  const closeAfter = (fn: () => void) => () => {
    fn();
    onClose();
  };

  const showInExplorer = closeAfter(() => {
    revealItemInDir(track.file_path).catch((err) => {
      console.error("[TrackContextMenu] revealItemInDir failed", err);
    });
  });

  const handleArtistClick = (artistId: number) =>
    closeAfter(() => onNavigateToArtist?.(artistId));

  return (
    <ContextMenu point={point} onClose={onClose}>
      <ContextMenuItem
        icon={<ListEnd size={14} />}
        label={t("trackActions.playNext")}
        onSelect={closeAfter(() => onPlayNext(track.id))}
      />
      <ContextMenuItem
        icon={<ListPlus size={14} />}
        label={t("trackActions.addToQueue")}
        onSelect={closeAfter(() => onAddToQueue(track.id))}
      />

      <ContextMenuSeparator />

      <ContextMenuSub
        icon={<Plus size={14} />}
        label={t("trackActions.addToPlaylist")}
      >
        <div className="max-h-64 overflow-y-auto">
          {playlists.length === 0 ? (
            <div className="px-3 py-2 text-xs text-zinc-400">
              {t("trackActions.noPlaylists")}
            </div>
          ) : (
            playlists.map((pl) => {
              const color = resolvePlaylistColor(pl.color_id);
              return (
                <button
                  key={pl.id}
                  type="button"
                  role="menuitem"
                  onClick={closeAfter(() => onAddToPlaylist(pl.id, track.id))}
                  className="w-full flex items-center gap-2 px-3 py-2 text-left text-sm text-zinc-700 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
                >
                  <span
                    className={`w-6 h-6 rounded-md flex items-center justify-center shrink-0 ${color.tileBg} ${color.tileText}`}
                  >
                    <PlaylistIcon iconId={pl.icon_id} size={12} />
                  </span>
                  <span className="truncate">{pl.name}</span>
                </button>
              );
            })
          )}
        </div>
        <ContextMenuSeparator />
        <ContextMenuItem
          icon={<Plus size={14} />}
          label={t("trackActions.createPlaylist")}
          onSelect={closeAfter(onCreatePlaylist)}
        />
      </ContextMenuSub>

      <ContextMenuItem
        icon={
          <Heart size={14} className={isLiked ? "fill-current text-pink-500" : ""} />
        }
        label={isLiked ? t("trackActions.unlike") : t("trackActions.like")}
        onSelect={closeAfter(() => onToggleLike(track.id))}
      />

      {currentPlaylistId != null && onRemoveFromPlaylist != null && (
        <ContextMenuItem
          icon={<Trash2 size={14} />}
          label={t("trackActions.removeFromPlaylist")}
          danger
          onSelect={closeAfter(() =>
            onRemoveFromPlaylist(currentPlaylistId, track.id),
          )}
        />
      )}

      {(track.album_id != null || artists.length > 0) && <ContextMenuSeparator />}

      {track.album_id != null && onNavigateToAlbum != null && (
        <ContextMenuItem
          icon={<Disc3 size={14} />}
          label={t("trackActions.goToAlbum")}
          onSelect={closeAfter(() => onNavigateToAlbum(track.album_id!))}
        />
      )}

      {artists.length === 1 && onNavigateToArtist != null && (
        <ContextMenuItem
          icon={<User size={14} />}
          label={t("trackActions.goToArtist")}
          onSelect={handleArtistClick(artists[0].id)}
        />
      )}

      {artists.length > 1 && onNavigateToArtist != null && (
        <ContextMenuSub icon={<User size={14} />} label={t("trackActions.goToArtist")}>
          {artists.map((artist) => (
            <button
              key={artist.id}
              type="button"
              role="menuitem"
              onClick={handleArtistClick(artist.id)}
              className="w-full px-3 py-2 text-left text-sm text-zinc-700 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 truncate"
            >
              {artist.name}
            </button>
          ))}
        </ContextMenuSub>
      )}

      <ContextMenuSeparator />

      <ContextMenuItem
        icon={<ExternalLink size={14} />}
        label={t("trackActions.showInExplorer")}
        onSelect={showInExplorer}
      />
      <ContextMenuItem
        icon={<Info size={14} />}
        label={t("trackActions.properties")}
        onSelect={closeAfter(() => onShowProperties(track))}
      />
    </ContextMenu>
  );
}

/**
 * Pair the parallel `artist_ids` (comma-separated numeric IDs) with
 * the matching `artist_name` (`", "`-joined names) into individual
 * entries. Mismatched lengths fall back to "no artists" so we don't
 * navigate to the wrong page.
 */
function parseArtistList(
  idsStr: string | null,
  nameStr: string | null,
): ArtistEntry[] {
  if (!idsStr || !nameStr) return [];
  const ids = idsStr.split(",").map((s) => Number(s.trim())).filter((n) => Number.isFinite(n));
  const names = nameStr.split(", ").map((s) => s.trim()).filter(Boolean);
  if (ids.length === 0 || ids.length !== names.length) return [];
  return ids.map((id, i) => ({ id, name: names[i] }));
}

import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Play,
  ListPlus,
  ListEnd,
  Plus,
  Trash2,
  X,
} from "lucide-react";
import {
  playerAddToQueue,
  playerPlayNext,
  playerPlayTracks,
} from "../../lib/tauri/player";
import { addTracksToPlaylist, removeTrackFromPlaylist } from "../../lib/tauri/playlist";
import { usePlaylist } from "../../hooks/usePlaylist";
import { resolvePlaylistColor } from "../../lib/playlistVisuals";
import { PlaylistIcon } from "../../lib/PlaylistIcon";
import { Tooltip } from "./Tooltip";

export type SelectionContext =
  | { type: "playlist"; playlistId: number }
  | { type: "album"; albumId: number }
  | null;

interface SelectionActionBarProps {
  trackIds: number[];
  context?: SelectionContext;
  onClear: () => void;
  onCreatePlaylist?: () => void;
  /** Optional: parent can refresh its track list after a remove. */
  onAfterRemoveFromPlaylist?: (removedIds: number[]) => void;
}

export function SelectionActionBar({
  trackIds,
  context,
  onClear,
  onCreatePlaylist,
  onAfterRemoveFromPlaylist,
}: SelectionActionBarProps) {
  const { t } = useTranslation();
  const { playlists } = usePlaylist();
  const [isAddOpen, setIsAddOpen] = useState(false);
  const addRef = useRef<HTMLDivElement>(null);

  const count = trackIds.length;

  useEffect(() => {
    if (count === 0) return;
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (isAddOpen) setIsAddOpen(false);
        else onClear();
      }
    };
    document.addEventListener("keydown", handleKey);
    return () => document.removeEventListener("keydown", handleKey);
  }, [count, isAddOpen, onClear]);

  useEffect(() => {
    if (!isAddOpen) return;
    const handleMouseDown = (e: MouseEvent) => {
      const target = e.target as HTMLElement;
      if (addRef.current && addRef.current.contains(target)) return;
      if (target.closest("[data-selection-add-trigger]")) return;
      setIsAddOpen(false);
    };
    document.addEventListener("mousedown", handleMouseDown);
    return () => document.removeEventListener("mousedown", handleMouseDown);
  }, [isAddOpen]);

  if (count === 0) return null;

  const handlePlay = () => {
    playerPlayTracks("manual", null, trackIds, 0).catch((err) =>
      console.error("[SelectionActionBar] play failed", err),
    );
  };

  const handleAddToQueue = () => {
    playerAddToQueue(trackIds).catch((err) =>
      console.error("[SelectionActionBar] add to queue failed", err),
    );
  };

  const handlePlayNext = () => {
    playerPlayNext(trackIds).catch((err) =>
      console.error("[SelectionActionBar] play next failed", err),
    );
  };

  const handlePickPlaylist = async (playlistId: number) => {
    setIsAddOpen(false);
    try {
      await addTracksToPlaylist(playlistId, trackIds);
    } catch (err) {
      console.error("[SelectionActionBar] add to playlist failed", err);
    }
  };

  const handleCreatePlaylist = () => {
    setIsAddOpen(false);
    onCreatePlaylist?.();
  };

  const handleRemoveFromPlaylist = async () => {
    if (context?.type !== "playlist") return;
    const playlistId = context.playlistId;
    const ids = [...trackIds];
    try {
      await Promise.all(
        ids.map((id) => removeTrackFromPlaylist(playlistId, id)),
      );
      onAfterRemoveFromPlaylist?.(ids);
      onClear();
    } catch (err) {
      console.error("[SelectionActionBar] remove from playlist failed", err);
    }
  };

  return (
    <div
      role="toolbar"
      aria-label={t("selection.toolbarLabel")}
      className="fixed bottom-28 left-1/2 -translate-x-1/2 z-40 flex items-center gap-2 rounded-full border border-zinc-200 bg-white/95 backdrop-blur px-3 py-2 shadow-2xl dark:border-zinc-700 dark:bg-zinc-900/95"
    >
      <span className="px-2 text-sm font-semibold text-zinc-700 dark:text-zinc-200 tabular-nums">
        {t("selection.count", { count })}
      </span>

      <div className="h-5 w-px bg-zinc-200 dark:bg-zinc-700" aria-hidden="true" />

      <Tooltip label={t("selection.play")}>
        <button
          type="button"
          onClick={handlePlay}
          aria-label={t("selection.play")}
          className="p-2 rounded-full text-zinc-600 hover:text-emerald-600 hover:bg-emerald-50 dark:text-zinc-300 dark:hover:text-emerald-400 dark:hover:bg-emerald-500/10 transition-colors"
        >
          <Play size={16} className="fill-current" />
        </button>
      </Tooltip>

      <Tooltip label={t("selection.addToQueue")}>
        <button
          type="button"
          onClick={handleAddToQueue}
          aria-label={t("selection.addToQueue")}
          className="p-2 rounded-full text-zinc-600 hover:text-zinc-900 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:text-white dark:hover:bg-zinc-800 transition-colors"
        >
          <ListPlus size={16} />
        </button>
      </Tooltip>

      <Tooltip label={t("selection.playNext")}>
        <button
          type="button"
          onClick={handlePlayNext}
          aria-label={t("selection.playNext")}
          className="p-2 rounded-full text-zinc-600 hover:text-zinc-900 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:text-white dark:hover:bg-zinc-800 transition-colors"
        >
          <ListEnd size={16} />
        </button>
      </Tooltip>

      <div className="relative">
        <Tooltip label={t("selection.addToPlaylist")}>
          <button
            type="button"
            data-selection-add-trigger
            onClick={() => setIsAddOpen((v) => !v)}
            aria-haspopup="menu"
            aria-expanded={isAddOpen}
            aria-label={t("selection.addToPlaylist")}
            className={`p-2 rounded-full transition-colors ${
              isAddOpen
                ? "bg-emerald-500 text-white"
                : "text-zinc-600 hover:text-zinc-900 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:text-white dark:hover:bg-zinc-800"
            }`}
          >
            <Plus size={16} />
          </button>
        </Tooltip>
        {isAddOpen && (
          <div
            ref={addRef}
            role="menu"
            className="absolute bottom-full right-0 mb-2 z-50 w-60 rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated dark:shadow-black/40 overflow-hidden animate-fade-in"
          >
            <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase px-3 pt-3 pb-2">
              {t("selection.addToPlaylist")}
            </div>
            <div className="px-2 max-h-64 overflow-y-auto">
              {playlists.length === 0 ? (
                <div className="px-2 py-3 text-xs text-zinc-400 text-center">
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
                      onClick={() => handlePickPlaylist(pl.id)}
                      className="w-full flex items-center space-x-2 p-2 rounded-lg text-left hover:bg-zinc-50 dark:hover:bg-zinc-700/30 transition-colors"
                    >
                      <div
                        className={`w-7 h-7 rounded-md flex items-center justify-center shrink-0 ${color.tileBg} ${color.tileText}`}
                      >
                        <PlaylistIcon iconId={pl.icon_id} size={14} />
                      </div>
                      <span className="text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
                        {pl.name}
                      </span>
                    </button>
                  );
                })
              )}
            </div>
            {onCreatePlaylist && (
              <div className="border-t border-zinc-100 dark:border-zinc-700/50">
                <button
                  type="button"
                  role="menuitem"
                  onClick={handleCreatePlaylist}
                  className="w-full flex items-center space-x-2 px-3 py-2 text-left text-sm font-medium text-emerald-500 hover:bg-emerald-50 dark:hover:bg-emerald-900/20 transition-colors"
                >
                  <Plus size={14} />
                  <span>{t("trackActions.createPlaylist")}</span>
                </button>
              </div>
            )}
          </div>
        )}
      </div>

      {context?.type === "playlist" && (
        <Tooltip label={t("selection.removeFromPlaylist")}>
          <button
            type="button"
            onClick={handleRemoveFromPlaylist}
            aria-label={t("selection.removeFromPlaylist")}
            className="p-2 rounded-full text-red-500 hover:text-red-600 hover:bg-red-50 dark:hover:bg-red-500/10 transition-colors"
          >
            <Trash2 size={16} />
          </button>
        </Tooltip>
      )}

      <div className="h-5 w-px bg-zinc-200 dark:bg-zinc-700" aria-hidden="true" />

      <Tooltip label={t("selection.clear")}>
        <button
          type="button"
          onClick={onClear}
          aria-label={t("selection.clear")}
          className="p-2 rounded-full text-zinc-500 hover:text-zinc-900 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:text-white dark:hover:bg-zinc-800 transition-colors"
        >
          <X size={16} />
        </button>
      </Tooltip>
    </div>
  );
}

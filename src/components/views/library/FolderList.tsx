import { type FolderRow } from "../../../lib/tauri/browse";
import { type Translator } from "./libraryTypes";
import { type Playlist } from "../../../lib/tauri/playlist";
import { useEffect, useState } from "react";
import { countFolderPlayEvents } from "../../../lib/tauri/library";
import {
  Eye,
  EyeOff,
  Folder,
  Loader2,
  Plus,
  RefreshCcw,
  Trash2,
} from "lucide-react";
import { Tooltip } from "../../common/Tooltip";
import { AddToPlaylistPopover } from "./AddToPlaylistPopover";

interface FolderListProps {
  /** Descend into a root and start browsing it (#578). */
  onOpen: (path: string) => void;
  folders: FolderRow[];
  isLoading: boolean;
  t: Translator;
  playlists: Playlist[];
  onAddToPlaylist: (playlistId: number, folderId: number) => void;
  onCreatePlaylist: (folderId: number) => void;
  onToggleWatched: (folderId: number, enable: boolean) => void;
  onRemove: (folderId: number) => void;
  /** Issue #366 — force a full re-hash/re-read of every file in the
   *  folder, bypassing the normal (mtime, size) fast path. */
  onDeepRescan: (folderId: number) => void;
  /** The folder a deep rescan is currently running against, if any. */
  deepRescanFolderId: number | null;
  /** True while ANY scan is running — a library-wide pass included, not
   *  just a folder one. SQLite takes a single writer, so this row's
   *  button has to stand down for the global controls too. */
  isAnyRescanActive: boolean;
}

/** The last segment of a root's path: the name a card leads with, the
 *  full path staying underneath to tell two roots of one name apart. */
function folderLeaf(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  const cut = Math.max(trimmed.lastIndexOf("\\"), trimmed.lastIndexOf("/"));
  // A root such as "/" trims to nothing: the path itself is its name.
  return (cut >= 0 ? trimmed.slice(cut + 1) : trimmed) || trimmed || path;
}

export function FolderList({
  folders,
  onOpen,
  isLoading,
  t,
  playlists,
  onAddToPlaylist,
  onCreatePlaylist,
  onToggleWatched,
  onRemove,
  onDeepRescan,
  deepRescanFolderId,
  isAnyRescanActive,
}: FolderListProps) {
  const [openMenuFolderId, setOpenMenuFolderId] = useState<number | null>(null);
  // Two-step delete: first click arms the confirm state, second click
  // commits. Auto-clears after 3 s so the button doesn't stay armed
  // forever after the user wandered off.
  const [confirmDeleteId, setConfirmDeleteId] = useState<number | null>(null);
  // Listening events attached to the armed folder. Fetched on arm rather
  // than up-front so listing folders stays one query. Keyed by folder id
  // so a result for a folder that is no longer armed is simply ignored at
  // render — no reset pass, and nothing to clear when disarming.
  const [confirmPlayCount, setConfirmPlayCount] = useState<{
    folderId: number;
    count: number;
  } | null>(null);
  useEffect(() => {
    if (confirmDeleteId == null) return;
    const timer = setTimeout(() => setConfirmDeleteId(null), 3_000);
    return () => clearTimeout(timer);
  }, [confirmDeleteId]);

  // Resolve how much history the armed folder carries. The cancel flag
  // drops a response whose folder was disarmed mid-flight; the folder id
  // travels with the result so a late one can never be shown against a
  // different folder either.
  useEffect(() => {
    if (confirmDeleteId == null) return;
    const folderId = confirmDeleteId;
    let cancelled = false;
    void (async () => {
      try {
        const count = await countFolderPlayEvents(folderId);
        if (!cancelled) setConfirmPlayCount({ folderId, count });
      } catch (err) {
        // Non-fatal: the confirm falls back to its plain copy.
        console.error("[LibraryView] count folder play events failed", err);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [confirmDeleteId]);

  useEffect(() => {
    if (openMenuFolderId == null) return;
    const handleMouseDown = (event: MouseEvent) => {
      const target = event.target as HTMLElement;
      if (target.closest("[data-add-to-playlist-popover]")) return;
      if (target.closest("[data-add-to-playlist-trigger]")) return;
      setOpenMenuFolderId(null);
    };
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpenMenuFolderId(null);
    };
    document.addEventListener("mousedown", handleMouseDown);
    document.addEventListener("keydown", handleEscape);
    return () => {
      document.removeEventListener("mousedown", handleMouseDown);
      document.removeEventListener("keydown", handleEscape);
    };
  }, [openMenuFolderId]);

  const formatScannedAt = (ts: number | null): string => {
    if (ts == null) return t("library.folderList.neverScanned");
    const d = new Date(ts);
    return d.toLocaleString();
  };
  return (
    <div
      className={`grid grid-cols-[repeat(auto-fill,minmax(17rem,1fr))] gap-4 ${
        isLoading ? "opacity-50" : ""
      }`}
    >
      {folders.map((folder) => {
        const isMenuOpen = openMenuFolderId === folder.id;
        return (
          <div
            key={folder.id}
            data-folder-card
            // The whole card opens the folder (#733): only the path text
            // did, with nothing but an underline on hover to say so, and a
            // tester on 1.8.0-beta.1 never found the folder browser. A
            // click that starts on one of the card's own controls — or in
            // the playlist popover rendered inside it — is theirs, not the
            // card's. The name stays a button, for the keyboard.
            onClick={(e) => {
              const target = e.target as HTMLElement;
              if (
                target.closest(
                  "button, a, input, [role='menu'], [role='dialog']",
                )
              )
                return;
              onOpen(folder.path);
            }}
            className="group relative flex flex-col gap-3 rounded-2xl border border-zinc-200 bg-white p-4 transition-colors cursor-pointer hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-800/40 dark:hover:bg-zinc-800/70"
          >
            {/* The folder's tile on the left, its actions on the right:
                shown on hover or keyboard focus, the watch eye always when it is
                on. */}
            <div className="flex items-start justify-between gap-2">
              <div className="w-11 h-11 rounded-xl bg-blue-100 text-blue-600 dark:bg-blue-950/60 dark:text-blue-400 flex items-center justify-center shrink-0">
                <Folder size={22} />
              </div>
              <div className="flex items-center gap-0.5 -mr-1.5 -mt-1.5">
                <Tooltip
                  label={
                    folder.is_watched === 1
                      ? t("library.folderList.watchOff")
                      : t("library.folderList.watchOn")
                  }
                >
                  <button
                    type="button"
                    onClick={(e) => {
                      e.stopPropagation();
                      onToggleWatched(folder.id, folder.is_watched !== 1);
                    }}
                    aria-label={
                      folder.is_watched === 1
                        ? t("library.folderList.watchOff")
                        : t("library.folderList.watchOn")
                    }
                    aria-pressed={folder.is_watched === 1}
                    className={`p-1.5 rounded-full transition-colors ${
                      folder.is_watched === 1
                        ? "text-emerald-500 hover:text-emerald-600 hover:bg-emerald-50 dark:hover:bg-emerald-500/10"
                        : "opacity-0 reveal:opacity-100 text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-700"
                    }`}
                  >
                    {folder.is_watched === 1 ? (
                      <Eye size={16} />
                    ) : (
                      <EyeOff size={16} />
                    )}
                  </button>
                </Tooltip>
                <Tooltip label={t("library.folderList.deepRescan")}>
                  <button
                    type="button"
                    onClick={(e) => {
                      e.stopPropagation();
                      onDeepRescan(folder.id);
                    }}
                    disabled={isAnyRescanActive}
                    aria-label={t("library.folderList.deepRescan")}
                    aria-busy={deepRescanFolderId === folder.id}
                    className={`p-1.5 rounded-full transition-colors text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-700 disabled:opacity-50 ${
                      deepRescanFolderId === folder.id
                        ? "opacity-100"
                        : "opacity-0 reveal:opacity-100"
                    }`}
                  >
                    {deepRescanFolderId === folder.id ? (
                      <Loader2 size={16} className="animate-spin" />
                    ) : (
                      <RefreshCcw size={16} />
                    )}
                  </button>
                </Tooltip>
                <Tooltip
                  label={
                    confirmDeleteId === folder.id
                      ? confirmPlayCount?.folderId === folder.id &&
                        confirmPlayCount.count > 0
                        ? t("library.folderList.removeConfirmWithPlays", {
                            count: confirmPlayCount.count,
                          })
                        : t("library.folderList.removeConfirm")
                      : t("library.folderList.remove")
                  }
                >
                  <button
                    type="button"
                    onClick={(e) => {
                      e.stopPropagation();
                      if (confirmDeleteId === folder.id) {
                        setConfirmDeleteId(null);
                        onRemove(folder.id);
                      } else {
                        setConfirmDeleteId(folder.id);
                      }
                    }}
                    aria-label={
                      confirmDeleteId === folder.id
                        ? t("library.folderList.removeConfirm")
                        : t("library.folderList.remove")
                    }
                    className={`p-1.5 rounded-full transition-colors ${
                      confirmDeleteId === folder.id
                        ? "bg-red-500 text-white"
                        : "opacity-0 reveal:opacity-100 text-zinc-400 hover:text-red-500 hover:bg-red-50 dark:hover:bg-red-500/10"
                    }`}
                  >
                    <Trash2 size={16} />
                  </button>
                </Tooltip>
                <div className="relative">
                  <button
                    type="button"
                    data-add-to-playlist-trigger
                    onClick={(e) => {
                      e.stopPropagation();
                      setOpenMenuFolderId(isMenuOpen ? null : folder.id);
                    }}
                    aria-label={t("trackActions.addToPlaylist")}
                    className={`p-1.5 rounded-full transition-all ${
                      isMenuOpen
                        ? "opacity-100 bg-emerald-500 text-white"
                        : "opacity-0 reveal:opacity-100 text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-700"
                    }`}
                  >
                    <Plus size={16} />
                  </button>
                  {isMenuOpen && (
                    <AddToPlaylistPopover
                      playlists={playlists}
                      trackId={folder.id}
                      onPick={(playlistId) => {
                        onAddToPlaylist(playlistId, folder.id);
                        setOpenMenuFolderId(null);
                      }}
                      onCreate={() => {
                        setOpenMenuFolderId(null);
                        onCreatePlaylist(folder.id);
                      }}
                      t={t}
                    />
                  )}
                </div>{" "}
              </div>
            </div>
            <div className="min-w-0">
              <button
                type="button"
                onClick={() => onOpen(folder.path)}
                title={t("library.folderBrowser.open")}
                className="block w-full text-left text-sm font-semibold text-zinc-900 dark:text-white truncate hover:underline focus-visible:underline focus-visible:outline-none"
              >
                {folderLeaf(folder.path)}
              </button>
              <div
                className="text-[11px] text-zinc-500 truncate"
                title={folder.path}
              >
                {folder.path}
              </div>
            </div>
            <div className="flex items-center justify-between gap-2 text-xs text-zinc-500">
              <span className="shrink-0">
                {t("library.folderList.trackCount", {
                  count: folder.track_count,
                })}
              </span>
              <span className="truncate">
                {t("library.folderList.lastScanned", {
                  date: formatScannedAt(folder.last_scanned_at),
                })}
              </span>
            </div>
          </div>
        );
      })}
    </div>
  );
}

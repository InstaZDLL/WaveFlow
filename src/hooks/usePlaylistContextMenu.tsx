import {
  useCallback,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
} from "react";
import { useTranslation } from "react-i18next";
import {
  FileDown,
  FolderOpen,
  ListEnd,
  ListPlus,
  Pencil,
  Play,
  Trash2,
} from "lucide-react";
import {
  ContextMenu,
  ContextMenuItem,
  ContextMenuSeparator,
  type ContextMenuPoint,
} from "../components/common/ContextMenu";
import { isContextMenuKey, menuAnchorForElement } from "../lib/contextMenuKeys";
import { CreatePlaylistModal } from "../components/common/CreatePlaylistModal";
import { pickSaveFile } from "../lib/tauri/dialog";
import {
  exportPlaylistM3u,
  getPlaylist,
  listPlaylistTracks,
  updatePlaylist,
  type Playlist,
} from "../lib/tauri/playlist";
import {
  playerAddToQueue,
  playerPlayNext,
  playerPlayTracks,
} from "../lib/tauri/player";
import { usePlaylist } from "./usePlaylist";

/** The entry the menu acts on. Both surfaces that open it hold a
 *  `LibraryPlaylistRow`, whose `id` is text and may be a server id. */
export interface ContextPlaylist {
  /** Local rowid as text, or the server's identifier. */
  id: string;
  name: string;
  /** A server playlist has no local rowid, so it gets the short menu. */
  isRemote: boolean;
}

interface UsePlaylistContextMenuArgs {
  /** Called after a delete, so the caller can leave the page it was on. */
  onAfterDelete?: (playlistId: number) => void;
  /** Navigate to a server playlist. It is the only thing this menu can
   *  offer one — see the note on the remote branch below. */
  onOpenRemote?: (remotePlaylistId: string) => void;
}

/**
 * Right-click menu for a playlist, shared by the sidebar rows and the
 * library's cards.
 *
 * Every playlist action lived only as an icon button in the playlist's
 * own header, so reaching any of them meant opening the playlist first.
 * Right-clicking one did nothing: inert in a release build, and in a dev
 * build the webview's own menu came through instead (#737).
 *
 * A server playlist gets one item, Open. Every other action here takes a
 * local rowid it does not have, and its playback runs through the view's
 * own remote path rather than `player_play_tracks`. Offering the same
 * list greyed out would be a menu where nothing can be chosen, which is
 * worse than the inert right-click this replaces — so it offers the one
 * thing that does work and leads to where the rest already live.
 */
export function usePlaylistContextMenu({
  onAfterDelete,
  onOpenRemote,
}: UsePlaylistContextMenuArgs = {}) {
  const { t } = useTranslation();
  const { deletePlaylist, refresh } = usePlaylist();
  const [state, setState] = useState<{
    point: ContextMenuPoint;
    playlist: ContextPlaylist;
  } | null>(null);
  /** The playlist being edited, once its full row has been fetched —
   *  the menu only carries the summary the lists render. */
  const [editing, setEditing] = useState<Playlist | null>(null);

  const open = useCallback(
    (event: ReactMouseEvent, playlist: ContextPlaylist) => {
      event.preventDefault();
      event.stopPropagation();
      setState({ point: { x: event.clientX, y: event.clientY }, playlist });
    },
    [],
  );

  /** Keyboard counterpart: the Menu key or Shift+F10 on a focused row,
   *  anchored to the row since there is no pointer to place it at.
   *  Returns `true` when it handled the event, so the row's `onKeyDown`
   *  early-returns rather than re-deriving the condition. */
  const openFromKeyboard = useCallback(
    (event: ReactKeyboardEvent, playlist: ContextPlaylist): boolean => {
      if (!isContextMenuKey(event)) return false;
      event.preventDefault();
      event.stopPropagation();
      setState({
        point: menuAnchorForElement(event.currentTarget as HTMLElement),
        playlist,
      });
      return true;
    },
    [],
  );

  const close = useCallback(() => setState(null), []);

  const render = useCallback(() => {
    const menu = (() => {
      if (!state) return null;
      const { playlist } = state;
      const localId = playlist.isRemote ? null : Number(playlist.id);
      const act = (run: () => void) => () => {
        run();
        close();
      };

      // Everything past here needs a local rowid, so the remote case is
      // its own short menu rather than the same one with every item
      // switched off.
      if (localId == null) {
        return (
          <ContextMenu point={state.point} onClose={close}>
            <ContextMenuItem
              icon={<FolderOpen size={16} aria-hidden="true" />}
              label={t("common.open")}
              onSelect={act(() => onOpenRemote?.(playlist.id))}
            />
          </ContextMenu>
        );
      }

      const withTracks = (use: (ids: number[]) => Promise<void>) => () => {
        listPlaylistTracks(localId)
          .then((tracks) => {
            const ids = tracks.map((track) => track.id);
            if (ids.length === 0) return;
            return use(ids);
          })
          .catch((err: unknown) =>
            console.error("[usePlaylistContextMenu] queue failed", err),
          );
      };

      return (
        <ContextMenu point={state.point} onClose={close}>
          <ContextMenuItem
            icon={<Play size={16} aria-hidden="true" />}
            label={t("playlistView.actions.play")}
            onSelect={act(
              withTracks((ids) =>
                playerPlayTracks("playlist", localId, ids, 0),
              ),
            )}
          />
          <ContextMenuItem
            icon={<ListEnd size={16} aria-hidden="true" />}
            label={t("trackActions.playNext")}
            onSelect={act(withTracks(playerPlayNext))}
          />
          <ContextMenuItem
            icon={<ListPlus size={16} aria-hidden="true" />}
            label={t("trackActions.addToQueue")}
            onSelect={act(withTracks(playerAddToQueue))}
          />
          <ContextMenuSeparator />
          <ContextMenuItem
            icon={<Pencil size={16} aria-hidden="true" />}
            label={t("playlistView.actions.edit")}
            onSelect={act(() => {
              getPlaylist(localId)
                .then(setEditing)
                .catch((err: unknown) =>
                  console.error(
                    "[usePlaylistContextMenu] load for edit failed",
                    err,
                  ),
                );
            })}
          />
          <ContextMenuItem
            icon={<FileDown size={16} aria-hidden="true" />}
            label={t("playlistView.actions.exportM3u")}
            onSelect={act(() => {
              pickSaveFile(`${playlist.name}.m3u8`, ["m3u8", "m3u"])
                .then((dest) =>
                  dest ? exportPlaylistM3u(localId, dest) : undefined,
                )
                .catch((err: unknown) =>
                  console.error("[usePlaylistContextMenu] export failed", err),
                );
            })}
          />
          <ContextMenuSeparator />
          <ContextMenuItem
            icon={<Trash2 size={16} aria-hidden="true" />}
            label={t("playlistView.actions.delete")}
            danger
            onSelect={act(() => {
              // The header's button asks twice before deleting; a
              // menu item is already a deliberate second gesture,
              // and the entry it acted on stays visible until the
              // list refreshes, so the outcome is never a surprise.
              deletePlaylist(localId)
                .then(() => onAfterDelete?.(localId))
                .catch((err: unknown) =>
                  console.error("[usePlaylistContextMenu] delete failed", err),
                );
            })}
          />
        </ContextMenu>
      );
    })();

    return (
      <>
        {menu}
        {/* Mounted beside the menu rather than by each caller, so the
            sidebar and the grid cannot drift apart on what Edit opens. */}
        {editing && (
          <CreatePlaylistModal
            isOpen
            existing={editing}
            onClose={() => setEditing(null)}
            onCreate={async (data) => {
              await updatePlaylist(editing.id, {
                name: data.name,
                description: data.description,
                color_id: data.colorId,
                color_mode: data.colorMode,
                icon_id: data.iconId,
              });
              await refresh();
              setEditing(null);
            }}
            onCoverChanged={() => void refresh()}
          />
        )}
      </>
    );
  }, [
    state,
    close,
    editing,
    t,
    deletePlaylist,
    refresh,
    onAfterDelete,
    onOpenRemote,
  ]);

  return { open, openFromKeyboard, close, render };
}

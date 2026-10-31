import {
  useCallback,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
} from "react";
import { useTranslation } from "react-i18next";
import { ImageIcon, Pin, PinOff, Tags } from "lucide-react";

import {
  ContextMenu,
  ContextMenuItem,
  type ContextMenuPoint,
} from "../components/common/ContextMenu";
import { isContextMenuKey, menuAnchorForElement } from "../lib/contextMenuKeys";
import { openTagWorkshop } from "../lib/tagWorkshop";
import { toggleSidebarPin, useSidebarPins } from "./useSidebarPins";

/** A local album or artist, by rowid. A server album or artist has no
 *  rowid to pin, so its tile does not open this menu. */
export interface AlbumArtistMenuTarget {
  kind: "album" | "artist";
  id: number;
}

interface UseAlbumArtistContextMenuArgs {
  /** Offer "Change cover" on an album: the library grid passes it, where
   *  the cover picker is mounted. */
  onChangeCover?: (albumId: number) => void;
}

/**
 * Right-click menu for an album or an artist tile, shared by every place
 * one is drawn — the library grids, the artist page's discography and
 * similar artists, Home (both layouts), Statistics, Wrapped and the search
 * results — so a tile anywhere answers the same way. Before it only the
 * library grid had a menu, and a right-click anywhere else brought up the
 * webview's own (save image, inspect).
 *
 * `bind(target)` gives the handlers to spread on the tile: the right-click
 * and its keyboard counterpart (the Menu key, Shift+F10). The menu is the
 * portalled `ContextMenu`, so a tile under a `backdrop-filter` or a
 * transformed row cannot clip it.
 */
export function useAlbumArtistContextMenu({
  onChangeCover,
}: UseAlbumArtistContextMenuArgs = {}) {
  const { t } = useTranslation();
  const { isPinned } = useSidebarPins();
  const [state, setState] = useState<{
    point: ContextMenuPoint;
    target: AlbumArtistMenuTarget;
  } | null>(null);

  const close = useCallback(() => setState(null), []);

  const bind = useCallback(
    (target: AlbumArtistMenuTarget) => ({
      onContextMenu: (event: ReactMouseEvent) => {
        event.preventDefault();
        event.stopPropagation();
        setState({ point: { x: event.clientX, y: event.clientY }, target });
      },
      onKeyDown: (event: ReactKeyboardEvent) => {
        if (!isContextMenuKey(event)) return;
        event.preventDefault();
        event.stopPropagation();
        setState({
          point: menuAnchorForElement(event.currentTarget as HTMLElement),
          target,
        });
      },
    }),
    [],
  );

  const render = () => {
    if (!state) return null;
    const { kind, id } = state.target;
    const pinned = isPinned(kind, id);
    return (
      <ContextMenu point={state.point} onClose={close}>
        {kind === "album" && onChangeCover && (
          <ContextMenuItem
            icon={<ImageIcon size={16} aria-hidden="true" />}
            label={t("library.changeCover")}
            onSelect={() => {
              close();
              onChangeCover(id);
            }}
          />
        )}
        {kind === "album" && (
          <ContextMenuItem
            icon={<Tags size={16} aria-hidden="true" />}
            label={t("tagWorkshop.openFromMenu")}
            onSelect={() => {
              close();
              openTagWorkshop({ source: { kind: "album", album_id: id } });
            }}
          />
        )}
        <ContextMenuItem
          icon={
            pinned ? (
              <PinOff size={16} aria-hidden="true" />
            ) : (
              <Pin size={16} aria-hidden="true" />
            )
          }
          label={pinned ? t("sidebar.pins.unpin") : t("sidebar.pins.pin")}
          onSelect={() => {
            close();
            toggleSidebarPin(kind, id, !pinned).catch((err: unknown) =>
              console.error("[useAlbumArtistContextMenu] pin failed", err),
            );
          }}
        />
      </ContextMenu>
    );
  };

  /** A menu is open: a surface with keys of its own (Wrapped's Escape and
   *  Space, the search dropdown's Escape) leaves them to the menu. */
  const isOpen = state != null;

  return { bind, close, render, isOpen };
}

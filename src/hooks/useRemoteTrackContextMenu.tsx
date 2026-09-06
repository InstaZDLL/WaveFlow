import { useCallback, useState, type MouseEvent as ReactMouseEvent } from "react";
import { useTranslation } from "react-i18next";
import { Download, FolderInput, Pencil } from "lucide-react";
import {
  ContextMenu,
  ContextMenuItem,
  type ContextMenuPoint,
} from "../components/common/ContextMenu";

/** The row the menu acts on — only what the three actions need. */
export interface RemoteContextTrack {
  /** The server's identifier for the track. */
  remoteId: string;
  title: string;
}

interface UseRemoteTrackContextMenuArgs {
  /** Start a download of the server's copy. */
  onDownload: (remoteId: string) => void;
  /** Copy an already-downloaded track into a scanned folder. */
  onImport: (track: RemoteContextTrack) => void;
  /** Open the server-side tag editor. */
  onEditTags: (track: RemoteContextTrack) => void;
  /** Server ids currently downloading, to disable a second start. */
  downloading?: Set<string>;
  /** Server ids already held locally — import needs one of these. */
  downloaded?: Set<string>;
}

/**
 * Right-click menu for a track that lives on the server.
 *
 * The local menu is built around a `Track` — a local rowid, a file path, a
 * rating — and a server track has none of those. Rows resolved that mismatch
 * by handing the menu `null`, which meant right-clicking a server row did
 * nothing at all: in a release build the gesture was inert, and in a dev build
 * the webview's own menu came through it. Meanwhile the same three actions sat
 * behind hover-only icons, so a pointer that never hovered never found them.
 *
 * This offers exactly those three, so both gestures agree.
 */
export function useRemoteTrackContextMenu({
  onDownload,
  onImport,
  onEditTags,
  downloading,
  downloaded,
}: UseRemoteTrackContextMenuArgs) {
  const { t } = useTranslation();
  const [state, setState] = useState<{
    point: ContextMenuPoint;
    track: RemoteContextTrack;
  } | null>(null);

  const open = useCallback(
    (event: ReactMouseEvent, track: RemoteContextTrack) => {
      event.preventDefault();
      event.stopPropagation();
      setState({ point: { x: event.clientX, y: event.clientY }, track });
    },
    [],
  );

  const close = useCallback(() => setState(null), []);

  const render = useCallback(() => {
    if (!state) return null;
    const { track } = state;
    const isDownloading = downloading?.has(track.remoteId) ?? false;
    const isDownloaded = downloaded?.has(track.remoteId) ?? false;
    const act = (run: () => void) => () => {
      run();
      close();
    };
    return (
      <ContextMenu point={state.point} onClose={close}>
        <ContextMenuItem
          icon={<Download size={16} aria-hidden="true" />}
          label={isDownloaded ? t("remote.download.kept") : t("remote.download.keep")}
          disabled={isDownloading || isDownloaded}
          onSelect={act(() => onDownload(track.remoteId))}
        />
        <ContextMenuItem
          icon={<FolderInput size={16} aria-hidden="true" />}
          label={t("remote.import.action")}
          // Importing copies bytes that have to be here already.
          disabled={!isDownloaded}
          onSelect={act(() => onImport(track))}
        />
        <ContextMenuItem
          icon={<Pencil size={16} aria-hidden="true" />}
          label={t("remote.tags.action")}
          onSelect={act(() => onEditTags(track))}
        />
      </ContextMenu>
    );
  }, [state, close, downloading, downloaded, onDownload, onImport, onEditTags, t]);

  return { open, close, render };
}

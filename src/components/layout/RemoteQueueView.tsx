import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useVirtualizer } from "@tanstack/react-virtual";
import { usePlayer } from "../../hooks/usePlayer";
import {
  remoteGetPlayQueue,
  remoteQueueJump,
  type RemotePlayQueue,
} from "../../lib/tauri/remoteServer";
import { formatDuration } from "../../lib/tauri/track";
import { RemoteArtwork } from "../common/RemoteArtwork";

/** Row height (px) the Up Next virtualizer estimates with — the button's
 *  `p-2` (16px) around a `h-10` (40px) cover. */
const REMOTE_QUEUE_ROW_HEIGHT = 56;

/**
 * Queue panel body for a remote play session (RFC-005). The remote queue
 * lives in memory on the backend, not in the local `queue_item` table, so
 * it gets its own view rather than being forced through `player_get_queue`
 * (whose jump / reorder act on the local queue). Same shape as
 * {@link SpotifyQueueView}: Now Playing + Up Next, click a row to jump.
 *
 * Localized under the shared `remote.*` i18n namespace, like the rest of
 * the remote surface.
 */
export function RemoteQueueView() {
  const { t } = useTranslation();
  const { currentTrack } = usePlayer();
  const [queue, setQueue] = useState<RemotePlayQueue | null>(null);

  // Re-read whenever the playing track changes: an advance / jump moves the
  // backend cursor, and the negative sentinel id flips with it, so this
  // refetches the fresh index without a dedicated event.
  const currentId = currentTrack?.id ?? null;
  const seqRef = useRef(0);
  const refresh = useCallback(() => {
    const seq = ++seqRef.current;
    remoteGetPlayQueue()
      .then((q) => {
        if (seq === seqRef.current) setQueue(q);
      })
      .catch(() => {
        if (seq === seqRef.current) setQueue(null);
      });
  }, []);
  useEffect(() => {
    // `currentId` is a dependency on purpose — it is the change signal
    // (an advance / jump moves the cursor and flips the sentinel id).
    void currentId;
    refresh();
  }, [refresh, currentId]);

  const handleJump = useCallback((index: number) => {
    remoteQueueJump(index).catch((err) =>
      console.error("[RemoteQueueView] jump failed", err),
    );
  }, []);

  // Virtualize the Up Next list against its own scroll container (the
  // QueuePanel scrolls independently of the page). Hook runs
  // unconditionally, before the early return, with a count derived from the
  // queue so a remote session of any length renders ~a screenful of rows.
  const scrollRef = useRef<HTMLDivElement>(null);
  const upNextCount = queue
    ? Math.max(0, queue.entries.length - queue.index - 1)
    : 0;
  // eslint-disable-next-line react-hooks/incompatible-library
  const rowVirtualizer = useVirtualizer({
    count: upNextCount,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => REMOTE_QUEUE_ROW_HEIGHT,
    overscan: 6,
  });

  if (!queue || queue.entries.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center text-sm text-zinc-500">
        {t("remote.queue.empty")}
      </div>
    );
  }

  const nowPlaying = queue.entries[queue.index] ?? null;
  const upNextStart = queue.index + 1;

  return (
    <div className="flex-1 flex flex-col min-h-0 -mx-2 px-2 space-y-5">
      {nowPlaying && (
        <section className="shrink-0">
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-2 px-1">
            {t("remote.queue.nowPlaying")}
          </div>
          <RemoteQueueRow entry={nowPlaying} isCurrent />
        </section>
      )}
      {upNextCount > 0 && (
        <section className="flex-1 flex flex-col min-h-0">
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-2 px-1">
            {t("remote.queue.upNext")} · {upNextCount}
          </div>
          <div
            ref={scrollRef}
            className="flex-1 min-h-0 overflow-y-auto scrollbar-hide"
          >
            <div
              style={{
                height: `${rowVirtualizer.getTotalSize()}px`,
                position: "relative",
                width: "100%",
              }}
            >
              {rowVirtualizer.getVirtualItems().map((virtualRow) => {
                const absoluteIndex = upNextStart + virtualRow.index;
                const entry = queue.entries[absoluteIndex];
                if (!entry) return null;
                return (
                  <RemoteQueueRow
                    key={`${entry.id}-${absoluteIndex}`}
                    entry={entry}
                    top={virtualRow.start}
                    rowHeight={REMOTE_QUEUE_ROW_HEIGHT}
                    onJump={() => handleJump(absoluteIndex)}
                  />
                );
              })}
            </div>
          </div>
        </section>
      )}
    </div>
  );
}

function RemoteQueueRow({
  entry,
  isCurrent = false,
  onJump,
  top,
  rowHeight,
}: {
  entry: RemotePlayQueue["entries"][number];
  isCurrent?: boolean;
  onJump?: () => void;
  /** Absolute offset within the virtualized Up Next list. Omitted for the
   *  static Now Playing row. */
  top?: number;
  rowHeight?: number;
}) {
  const { t } = useTranslation();
  const style: React.CSSProperties | undefined =
    top != null && rowHeight != null
      ? {
          position: "absolute",
          top: `${top}px`,
          left: 0,
          width: "100%",
          height: `${rowHeight}px`,
        }
      : undefined;
  return (
    <button
      type="button"
      onClick={onJump}
      disabled={isCurrent}
      style={style}
      className={`w-full flex items-center space-x-3 p-2 rounded-lg text-left transition-colors select-none ${
        isCurrent
          ? "bg-emerald-50 dark:bg-emerald-900/20 cursor-default"
          : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
      }`}
    >
      <RemoteArtwork hash={entry.artwork_hash} className="w-10 h-10 rounded" iconSize={18} />
      <div className="flex-1 min-w-0">
        <div
          className={`text-sm truncate ${
            isCurrent
              ? "text-emerald-600 dark:text-emerald-400 font-semibold"
              : "text-zinc-800 dark:text-zinc-200"
          }`}
        >
          {entry.title ?? t("remote.common.awaitingMetadata")}
        </div>
        <div className="text-xs text-zinc-500 truncate">
          {entry.artist ?? "—"}
        </div>
      </div>
      {entry.duration_ms != null && (
        <div className="text-xs text-zinc-400 tabular-nums shrink-0">
          {formatDuration(entry.duration_ms)}
        </div>
      )}
    </button>
  );
}

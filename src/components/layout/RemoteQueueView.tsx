import { useCallback, useEffect, useRef, useState } from "react";
import { usePlayer } from "../../hooks/usePlayer";
import {
  remoteGetPlayQueue,
  remoteQueueJump,
  type RemotePlayQueue,
} from "../../lib/tauri/remoteServer";
import { formatDuration } from "../../lib/tauri/track";
import { RemoteArtwork } from "../common/RemoteArtwork";

/**
 * Queue panel body for a remote play session (RFC-005). The remote queue
 * lives in memory on the backend, not in the local `queue_item` table, so
 * it gets its own view rather than being forced through `player_get_queue`
 * (whose jump / reorder act on the local queue). Same shape as
 * {@link SpotifyQueueView}: Now Playing + Up Next, click a row to jump.
 *
 * Not localized — behind the same off-by-default `sync_v2` feature as the
 * rest of the remote surface.
 */
export function RemoteQueueView() {
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

  if (!queue || queue.entries.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center text-sm text-zinc-500">
        Nothing queued.
      </div>
    );
  }

  const nowPlaying = queue.entries[queue.index] ?? null;
  const upNext = queue.entries.slice(queue.index + 1);

  return (
    <div className="flex-1 flex flex-col min-h-0 -mx-2 px-2 space-y-5">
      {nowPlaying && (
        <section className="shrink-0">
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-2 px-1">
            Now playing
          </div>
          <RemoteQueueRow entry={nowPlaying} isCurrent />
        </section>
      )}
      {upNext.length > 0 && (
        <section className="flex-1 flex flex-col min-h-0">
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-2 px-1">
            Up next · {upNext.length}
          </div>
          <div className="flex-1 min-h-0 overflow-y-auto scrollbar-hide space-y-0.5">
            {upNext.map((entry, i) => {
              const absoluteIndex = queue.index + 1 + i;
              return (
                <RemoteQueueRow
                  key={`${entry.id}-${absoluteIndex}`}
                  entry={entry}
                  onJump={() => handleJump(absoluteIndex)}
                />
              );
            })}
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
}: {
  entry: RemotePlayQueue["entries"][number];
  isCurrent?: boolean;
  onJump?: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onJump}
      disabled={isCurrent}
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
          {entry.title ?? "Awaiting metadata…"}
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

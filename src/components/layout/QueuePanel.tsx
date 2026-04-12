import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { X, ListMusic } from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";
import { Artwork } from "../common/Artwork";
import {
  playerGetQueue,
  playerJumpToIndex,
  type PlayerQueueSnapshot,
  type QueueTrackPayload,
} from "../../lib/tauri/player";

/**
 * Right-hand queue panel. Shows:
 * - "Now playing" highlighted at current_index
 * - "Next up" as the remainder of the queue below
 *
 * Backed by `player_get_queue`, re-fetched at mount, when the panel
 * opens, and whenever a `player:queue-changed` event fires (the
 * backend emits this after fill_queue, advance, shuffle, etc).
 */
export function QueuePanel() {
  const { t } = useTranslation();
  const { isQueueOpen, toggleQueue, playbackState } = usePlayer();
  const [snapshot, setSnapshot] = useState<PlayerQueueSnapshot | null>(null);

  // Seq counter so overlapping refetches never resolve in the
  // wrong order. Quickly clicking Next fires many queue-changed
  // events; without this guard, an older fetch can return after a
  // newer one and overwrite the freshest state with stale data
  // (which is exactly the "PlayerBar says X, queue says Y"
  // desync bug).
  const fetchSeqRef = useRef(0);

  const doFetch = useCallback(() => {
    const seq = ++fetchSeqRef.current;
    playerGetQueue()
      .then((q) => {
        if (seq === fetchSeqRef.current) setSnapshot(q);
      })
      .catch((err) => {
        console.error("[QueuePanel] fetch failed", err);
        if (seq === fetchSeqRef.current) setSnapshot(null);
      });
  }, []);

  // Initial load + listen for backend-driven queue changes.
  useEffect(() => {
    doFetch();
    let unlisten: UnlistenFn | null = null;
    (async () => {
      try {
        unlisten = await listen("player:queue-changed", () => {
          doFetch();
        });
      } catch (err) {
        console.error("[QueuePanel] listen failed", err);
      }
    })();
    return () => {
      if (unlisten) unlisten();
    };
  }, [doFetch]);

  // Re-fetch when the panel is opened — defensive, in case the
  // event bus missed a mutation while the panel was closed.
  useEffect(() => {
    if (isQueueOpen) doFetch();
  }, [isQueueOpen, doFetch]);

  const items: QueueTrackPayload[] = snapshot?.items ?? [];
  const currentIndex = snapshot?.current_index ?? -1;
  const nowPlaying =
    currentIndex >= 0 && currentIndex < items.length
      ? items[currentIndex]
      : null;
  const upNext = items.slice(Math.max(0, currentIndex + 1));
  const total = items.length;

  const isActive = playbackState === "playing" || playbackState === "paused";

  return (
    <div
      className={`absolute top-0 right-0 h-full w-80 shadow-2xl transform transition-transform duration-300 z-40 border-l bg-white border-zinc-200 text-zinc-800 dark:bg-zinc-900 dark:border-zinc-800 dark:text-zinc-100
        ${isQueueOpen ? "translate-x-0" : "translate-x-full"}`}
    >
      <div className="p-6 flex flex-col h-full">
        <div className="flex items-center justify-between mb-6">
          <div>
            <h2 className="text-xl font-bold">{t("queue.title")}</h2>
            <p className="text-xs text-zinc-500 mt-1">
              {t("queue.count", { count: total })}
              {!isActive && (
                <span className="bg-zinc-200 dark:bg-zinc-700 text-[10px] px-2 py-0.5 rounded-full ml-2 font-medium">
                  {t("queue.inactive")}
                </span>
              )}
            </p>
          </div>
          <button
            type="button"
            onClick={toggleQueue}
            aria-label={t("common.close")}
            className="p-2 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-full transition-colors"
          >
            <X size={20} />
          </button>
        </div>

        {total === 0 ? (
          <div className="flex-1 flex flex-col items-center justify-center text-center">
            <div className="w-24 h-24 bg-zinc-100 dark:bg-zinc-800 rounded-2xl flex items-center justify-center mb-6 shadow-inner">
              <ListMusic
                size={40}
                className="text-zinc-300 dark:text-zinc-600"
                aria-hidden="true"
              />
            </div>
            <h3 className="font-semibold mb-2">{t("queue.emptyTitle")}</h3>
            <p className="text-sm text-zinc-500 max-w-50">
              {t("queue.emptyDescription")}
            </p>
          </div>
        ) : (
          <div className="flex-1 overflow-y-auto -mx-2 px-2 space-y-5 scrollbar-hide">
            {nowPlaying && (
              <section>
                <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-2 px-1">
                  {t("queue.nowPlaying")}
                </div>
                <QueueRow item={nowPlaying} isCurrent />
              </section>
            )}
            {upNext.length > 0 && (
              <section>
                <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-2 px-1">
                  {t("queue.upNext", { count: upNext.length })}
                </div>
                <ul className="space-y-1">
                  {upNext.map((item, i) => {
                    const absoluteIndex = currentIndex + 1 + i;
                    return (
                      <li key={`${item.id}-${absoluteIndex}`}>
                        <QueueRow
                          item={item}
                          onDoubleClick={() => {
                            playerJumpToIndex(absoluteIndex).catch((err) =>
                              console.error("[QueuePanel] jump failed", err)
                            );
                          }}
                        />
                      </li>
                    );
                  })}
                </ul>
              </section>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function QueueRow({
  item,
  isCurrent = false,
  onDoubleClick,
}: {
  item: QueueTrackPayload;
  isCurrent?: boolean;
  onDoubleClick?: () => void;
}) {
  return (
    <div
      onDoubleClick={onDoubleClick}
      className={`flex items-center space-x-3 p-2 rounded-lg transition-colors select-none ${
        onDoubleClick ? "cursor-pointer" : ""
      } ${
        isCurrent
          ? "bg-emerald-50 dark:bg-emerald-900/20"
          : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
      }`}
    >
      <Artwork
        path={item.artwork_path}
        className="w-10 h-10"
        iconSize={18}
        alt={item.album_title ?? item.title}
        rounded="md"
      />
      <div className="flex-1 min-w-0">
        <div
          className={`text-sm truncate ${
            isCurrent
              ? "text-emerald-600 dark:text-emerald-400 font-semibold"
              : "text-zinc-800 dark:text-zinc-200"
          }`}
        >
          {item.title}
        </div>
        <div className="text-xs text-zinc-500 truncate">
          {item.artist_name ?? "—"}
        </div>
      </div>
    </div>
  );
}

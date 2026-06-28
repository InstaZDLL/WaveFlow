import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { ListMusic, Radio } from "lucide-react";
import { Artwork } from "../common/Artwork";
import {
  playerGetQueue,
  playerJumpToIndex,
  type PlayerQueueSnapshot,
  type QueueTrackPayload,
} from "../../lib/tauri/player";

/**
 * Queue tab of the immersive control panel (issue #328 follow-up).
 * Shows the now-playing track + the up-next list; double-click (or
 * Enter) a row to jump. Styled white-on-dark for the immersive backdrop
 * — the side [`QueuePanel`](../layout/QueuePanel.tsx) keeps the
 * theme-aware styling + drag-reorder for the docked surface; this tab is
 * a read-mostly companion, so it reuses the same backend commands
 * (`player_get_queue` / `player_jump_to_index`) without the dnd weight.
 */
export function ImmersiveQueueTab() {
  const { t } = useTranslation();
  const [snapshot, setSnapshot] = useState<PlayerQueueSnapshot | null>(null);

  // Seq guard so overlapping refetches (rapid Next clicks fire many
  // `player:queue-changed` events) never resolve out of order.
  const fetchSeqRef = useRef(0);
  const doFetch = useCallback(() => {
    const seq = ++fetchSeqRef.current;
    playerGetQueue()
      .then((q) => {
        if (seq === fetchSeqRef.current) setSnapshot(q);
      })
      .catch((err) => {
        // A transient refetch failure shouldn't blank the last known
        // queue — just log and keep the previous snapshot on screen.
        console.error("[ImmersiveQueueTab] fetch failed", err);
      });
  }, []);

  useEffect(() => {
    doFetch();
    let unlisten: UnlistenFn | null = null;
    // Guard against the component unmounting before `listen` resolves —
    // otherwise `unlisten` is still null when cleanup runs and the
    // subscription leaks. If disposed by the time it resolves, tear it
    // down immediately.
    let disposed = false;
    (async () => {
      try {
        const fn = await listen("player:queue-changed", doFetch);
        if (disposed) fn();
        else unlisten = fn;
      } catch (err) {
        console.error("[ImmersiveQueueTab] listen failed", err);
      }
    })();
    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, [doFetch]);

  const items: QueueTrackPayload[] = useMemo(
    () => snapshot?.items ?? [],
    [snapshot],
  );
  const currentIndex = snapshot?.current_index ?? -1;
  const nowPlaying =
    currentIndex >= 0 && currentIndex < items.length
      ? items[currentIndex]
      : null;
  const upNext = useMemo(
    () => items.slice(Math.max(0, currentIndex + 1)),
    [items, currentIndex],
  );
  const isRadio = snapshot?.source_type === "radio";

  const handleJump = useCallback((absoluteIndex: number) => {
    playerJumpToIndex(absoluteIndex).catch((err) =>
      console.error("[ImmersiveQueueTab] jump failed", err),
    );
  }, []);

  if (items.length === 0) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center text-center text-white/60 px-8">
        <ListMusic size={48} className="mb-4 opacity-60" aria-hidden="true" />
        <p className="text-base">{t("queue.emptyTitle")}</p>
      </div>
    );
  }

  return (
    <div className="flex-1 overflow-y-auto px-6 pb-8 space-y-6">
      {isRadio && items[0] && (
        <div className="flex items-center gap-3 p-3 rounded-xl bg-white/5 border border-white/10">
          <div className="shrink-0 w-9 h-9 rounded-full bg-emerald-500/20 flex items-center justify-center text-emerald-300">
            <Radio size={18} />
          </div>
          <div className="min-w-0">
            <div className="text-[10px] font-bold tracking-widest uppercase text-emerald-300">
              {t("queue.radio.label")}
            </div>
            <div className="text-xs text-white/70 truncate">
              {t("queue.radio.basedOn", { title: items[0].title })}
            </div>
          </div>
        </div>
      )}

      {nowPlaying && (
        <section>
          <div className="text-[10px] font-bold tracking-widest text-white/40 uppercase mb-2 px-1">
            {t("queue.nowPlaying")}
          </div>
          <ImmersiveQueueRow item={nowPlaying} isCurrent />
        </section>
      )}

      {upNext.length > 0 && (
        <section>
          <div className="text-[10px] font-bold tracking-widest text-white/40 uppercase mb-2 px-1">
            {t("queue.upNext", { count: upNext.length })}
          </div>
          <div className="space-y-0.5">
            {upNext.map((item, i) => {
              const absoluteIndex = currentIndex + 1 + i;
              return (
                <ImmersiveQueueRow
                  key={absoluteIndex}
                  item={item}
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

function ImmersiveQueueRow({
  item,
  isCurrent = false,
  onJump,
}: {
  item: QueueTrackPayload;
  isCurrent?: boolean;
  onJump?: () => void;
}) {
  const interactive = !isCurrent && onJump != null;
  return (
    <div
      onDoubleClick={onJump}
      tabIndex={interactive ? 0 : undefined}
      role={interactive ? "button" : undefined}
      onKeyDown={
        interactive
          ? (e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onJump?.();
              }
            }
          : undefined
      }
      className={`flex items-center gap-3 p-2 rounded-lg transition-colors select-none ${
        isCurrent
          ? "bg-white/10"
          : "cursor-pointer hover:bg-white/10 focus:outline-none focus-visible:ring-2 focus-visible:ring-white/40"
      }`}
    >
      <Artwork
        path={item.artwork_path}
        path1x={item.artwork_path_1x}
        path2x={item.artwork_path_2x}
        size="1x"
        className="w-10 h-10 shrink-0"
        iconSize={18}
        alt={item.album_title ?? item.title}
        rounded="md"
      />
      <div className="flex-1 min-w-0">
        <div
          className={`text-sm truncate ${
            isCurrent ? "text-emerald-300 font-semibold" : "text-white/90"
          }`}
        >
          {item.title}
        </div>
        <div className="text-xs text-white/50 truncate">
          {item.artist_name ?? "—"}
        </div>
      </div>
    </div>
  );
}

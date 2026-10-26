import { useCallback, useMemo } from "react";
import { useTranslation } from "react-i18next";
import {
  isPlayClick,
  useSingleClickPlay,
} from "../../hooks/useSingleClickPlay";
import { ListMusic, Radio } from "lucide-react";
import { Artwork } from "../common/Artwork";
import { usePlayerQueue } from "../../hooks/usePlayerQueue";
import { usePlayer } from "../../hooks/usePlayer";
import {
  isFileTrack,
  isRadioTrack,
  isRemoteTrack,
} from "../../lib/playerSources";
import { RemoteQueueView } from "../layout/RemoteQueueView";
import {
  playerJumpToIndex,
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
 *
 * A remote play session and files played without importing them have
 * their own queue, shown by the docked panel's view, as there; a web
 * radio station plays outside the queue, so none of it is "now playing".
 */
export function ImmersiveQueueTab() {
  const { currentTrack } = usePlayer();
  if (isRemoteTrack(currentTrack) || isFileTrack(currentTrack)) {
    return (
      <div className="flex-1 min-h-0 flex flex-col px-6 pb-8">
        <RemoteQueueView
          source={isFileTrack(currentTrack) ? "file" : "remote"}
        />
      </div>
    );
  }
  return <LocalQueue onAir={isRadioTrack(currentTrack)} />;
}

/** The library queue. `onAir`: a web radio station plays instead, so
 *  none of it is the playing track. */
function LocalQueue({ onAir }: { onAir: boolean }) {
  const { t } = useTranslation();
  const { value: singleClickPlay } = useSingleClickPlay();
  const snapshot = usePlayerQueue();

  const items: QueueTrackPayload[] = useMemo(
    () => snapshot?.items ?? [],
    [snapshot],
  );
  const currentIndex = onAir ? -1 : (snapshot?.current_index ?? -1);
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
                  singleClickPlay={singleClickPlay}
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
  singleClickPlay = false,
  onJump,
}: {
  item: QueueTrackPayload;
  isCurrent?: boolean;
  /** Settings → single-click play, read once by the tab. */
  singleClickPlay?: boolean;
  onJump?: () => void;
}) {
  const interactive = !isCurrent && onJump != null;
  return (
    <div
      onClick={(e) => {
        if (onJump && isPlayClick(e, singleClickPlay)) onJump();
      }}
      onDoubleClick={() => {
        if (!singleClickPlay) onJump?.();
      }}
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

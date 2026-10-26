import { useCallback, useEffect, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useVirtualizer } from "@tanstack/react-virtual";
import { AudioLines, ListMusic } from "lucide-react";
import {
  isPlayClick,
  useSingleClickPlay,
} from "../../hooks/useSingleClickPlay";
import { usePlayerQueue } from "../../hooks/usePlayerQueue";
import { usePrefersReducedMotion } from "../../hooks/usePrefersReducedMotion";
import { usePlayer } from "../../hooks/usePlayer";
import {
  isFileTrack,
  isRadioTrack,
  isRemoteTrack,
} from "../../lib/playerSources";
import { RemoteQueueView } from "../layout/RemoteQueueView";
import { playerJumpToIndex } from "../../lib/tauri/player";
import { formatDuration } from "../../lib/tauri/track";
import { formatPlaylistDuration } from "../../lib/playlistDuration";

const ROW_HEIGHT = 56;

/**
 * Tracklist tab of the immersive control panel — the theater view's
 * second column. The whole queue in its order, which is the album or the
 * playlist being played: what has played dimmed, the playing track marked
 * and kept in the middle of the column, what comes next after it. The
 * Queue tab shows only what is still to come; this one is the record
 * sleeve, numbered, with the total length above it.
 *
 * Virtualized against its own scroller: the view is an overlay over the
 * page, and a queue started from the library can hold thousands of rows.
 *
 * A remote play session and files played without importing them have
 * their own queue, shown by the docked panel's view, as there; a web
 * radio station plays outside the queue, so no row is marked playing.
 */
export function ImmersiveTracklistTab() {
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
  return <LocalTracklist onAir={isRadioTrack(currentTrack)} />;
}

/** The library queue as a tracklist. `onAir`: a web radio station plays
 *  instead, so nothing in it is the playing track. */
function LocalTracklist({ onAir }: { onAir: boolean }) {
  "use no memo";
  const { t } = useTranslation();
  const { value: singleClickPlay } = useSingleClickPlay();
  const reducedMotion = usePrefersReducedMotion();
  const snapshot = usePlayerQueue();
  const items = useMemo(() => snapshot?.items ?? [], [snapshot]);
  const currentIndex = onAir ? -1 : (snapshot?.current_index ?? -1);
  const totalMs = useMemo(
    () => items.reduce((sum, item) => sum + item.duration_ms, 0),
    [items],
  );

  const scrollRef = useRef<HTMLDivElement>(null);
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 8,
  });

  // Keep the playing track in the middle as the album advances. The first
  // placement jumps; later ones glide, unless motion is reduced.
  const placedRef = useRef(false);
  useEffect(() => {
    if (currentIndex < 0 || currentIndex >= items.length) return;
    virtualizer.scrollToIndex(currentIndex, {
      align: "center",
      behavior: placedRef.current && !reducedMotion ? "smooth" : "auto",
    });
    placedRef.current = true;
  }, [currentIndex, items.length, reducedMotion, virtualizer]);

  const handleJump = useCallback((absoluteIndex: number) => {
    playerJumpToIndex(absoluteIndex).catch((err) =>
      console.error("[ImmersiveTracklistTab] jump failed", err),
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
    <div className="flex-1 min-h-0 flex flex-col">
      <div className="shrink-0 flex items-baseline justify-between gap-4 px-7 pb-3 text-[10px] font-bold tracking-widest text-white/40 uppercase">
        <span>
          {currentIndex >= 0
            ? t("immersive.tracklist.position", {
                current: currentIndex + 1,
                total: items.length,
              })
            : null}
        </span>
        <span className="tabular-nums">
          {formatPlaylistDuration(totalMs, t)}
        </span>
      </div>
      <div ref={scrollRef} className="flex-1 min-h-0 overflow-y-auto px-6 pb-8">
        <ol
          aria-label={t("immersive.tracklist.tab")}
          className="relative"
          style={{ height: virtualizer.getTotalSize() }}
        >
          {virtualizer.getVirtualItems().map((row) => {
            const item = items[row.index];
            const isCurrent = row.index === currentIndex;
            const played = currentIndex >= 0 && row.index < currentIndex;
            return (
              <li
                key={row.key}
                className="absolute left-0 w-full"
                style={{
                  top: 0,
                  height: ROW_HEIGHT,
                  transform: `translateY(${row.start}px)`,
                }}
              >
                <div
                  onClick={(e) => {
                    if (!isCurrent && isPlayClick(e, singleClickPlay))
                      handleJump(row.index);
                  }}
                  onDoubleClick={() => {
                    if (!isCurrent && !singleClickPlay) handleJump(row.index);
                  }}
                  tabIndex={isCurrent ? undefined : 0}
                  role={isCurrent ? undefined : "button"}
                  aria-current={isCurrent ? "true" : undefined}
                  onKeyDown={
                    isCurrent
                      ? undefined
                      : (e) => {
                          if (e.key === "Enter" || e.key === " ") {
                            e.preventDefault();
                            handleJump(row.index);
                          }
                        }
                  }
                  className={`h-full flex items-center gap-4 px-3 rounded-lg select-none transition-colors ${
                    isCurrent
                      ? "bg-white/10"
                      : `cursor-pointer hover:bg-white/10 focus:outline-none focus-visible:ring-2 focus-visible:ring-white/40 ${
                          played ? "opacity-45" : ""
                        }`
                  }`}
                >
                  <span className="w-7 shrink-0 text-right text-sm tabular-nums text-white/50">
                    {isCurrent ? (
                      <AudioLines
                        size={16}
                        className="inline text-emerald-300"
                        aria-hidden="true"
                      />
                    ) : (
                      row.index + 1
                    )}
                  </span>
                  <span className="flex-1 min-w-0">
                    <span
                      className={`block text-sm truncate ${
                        isCurrent
                          ? "text-emerald-300 font-semibold"
                          : "text-white/90"
                      }`}
                    >
                      {item.title}
                    </span>
                    <span className="block text-xs text-white/50 truncate">
                      {item.artist_name ?? "—"}
                    </span>
                  </span>
                  <span className="shrink-0 text-xs tabular-nums text-white/50">
                    {formatDuration(item.duration_ms)}
                  </span>
                </div>
              </li>
            );
          })}
        </ol>
      </div>
    </div>
  );
}

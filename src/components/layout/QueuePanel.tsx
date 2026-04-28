import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { X, ListMusic, GripVertical } from "lucide-react";
import {
  DndContext,
  DragOverlay,
  MeasuringStrategy,
  PointerSensor,
  useSensor,
  useSensors,
  closestCenter,
  type DragEndEvent,
  type DragStartEvent,
} from "@dnd-kit/core";
import {
  arrayMove,
  SortableContext,
  useSortable,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { restrictToVerticalAxis } from "@dnd-kit/modifiers";
import { CSS } from "@dnd-kit/utilities";
import { useVirtualizer } from "@tanstack/react-virtual";
import { usePlayer } from "../../hooks/usePlayer";
import { Artwork } from "../common/Artwork";
import {
  playerGetQueue,
  playerJumpToIndex,
  playerReorderQueue,
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
  // Suppression window for `player:queue-changed`-driven refetches
  // right after our own optimistic reorder. The backend echoes the
  // event we caused, but its payload is identical to what we
  // already applied locally — refetching just creates new object
  // refs that bust memoization on every queue row mid-drag.
  const suppressFetchUntilRef = useRef(0);

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
          if (Date.now() < suppressFetchUntilRef.current) return;
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

  // Stable reference for `items` — `?? []` would otherwise create a
  // fresh array each render, busting every downstream useMemo even
  // when `snapshot` itself is unchanged.
  const items: QueueTrackPayload[] = useMemo(
    () => snapshot?.items ?? [],
    [snapshot],
  );
  const currentIndex = snapshot?.current_index ?? -1;
  const nowPlaying =
    currentIndex >= 0 && currentIndex < items.length
      ? items[currentIndex]
      : null;
  // Up Next is everything after the currently playing item. Virtualization
  // (see `SortableUpNext`) makes the full list cheap, so no slice / cap is
  // needed any more.
  const upNext = useMemo(
    () => items.slice(Math.max(0, currentIndex + 1)),
    [items, currentIndex],
  );
  const total = items.length;

  const handleJump = useCallback((absoluteIndex: number) => {
    playerJumpToIndex(absoluteIndex).catch((err) =>
      console.error("[QueuePanel] jump failed", err),
    );
  }, []);

  const handleReorder = useCallback(
    (fromAbs: number, toAbs: number) => {
      // Suppress the backend's echoed `queue-changed` event for the
      // next 300 ms — its payload would be identical to our local
      // reorder, so refetching only creates new object refs that
      // bust memoization on every row.
      suppressFetchUntilRef.current = Date.now() + 300;
      // Optimistic local reorder so the row settles in place before
      // the backend ack.
      setSnapshot((prev) => {
        if (!prev) return prev;
        const reordered = arrayMove(prev.items, fromAbs, toAbs);
        let nextIdx = prev.current_index;
        if (nextIdx === fromAbs) nextIdx = toAbs;
        else if (fromAbs < toAbs && nextIdx > fromAbs && nextIdx <= toAbs)
          nextIdx -= 1;
        else if (toAbs < fromAbs && nextIdx >= toAbs && nextIdx < fromAbs)
          nextIdx += 1;
        return {
          ...prev,
          items: reordered,
          current_index: nextIdx,
        };
      });
      playerReorderQueue(fromAbs, toAbs).catch((err) => {
        console.error("[QueuePanel] reorder failed", err);
        doFetch();
      });
    },
    [doFetch],
  );

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
                <SortableUpNext
                  items={upNext}
                  startIndex={currentIndex + 1}
                  onJump={handleJump}
                  onReorder={handleReorder}
                />
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

interface SortableUpNextProps {
  items: QueueTrackPayload[];
  /** Absolute queue index of `items[0]` (i.e. `currentIndex + 1`). */
  startIndex: number;
  onJump: (absoluteIndex: number) => void;
  onReorder: (fromAbsolute: number, toAbsolute: number) => void;
}

/**
 * Up-next list wrapped in a dnd-kit `DndContext`. Items are keyed by
 * their absolute queue position, which is stable across the optimistic
 * reorder because the parent re-derives `upNext` from a freshly
 * `arrayMove`d snapshot before the next render.
 *
 * Pointer activation distance avoids hijacking double-click — clicking
 * a row jumps to that track, only an actual drag past 4 px starts the
 * sort.
 */
const QUEUE_ROW_HEIGHT = 56;

function SortableUpNext({
  items,
  startIndex,
  onJump,
  onReorder,
}: SortableUpNextProps) {
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 4 } }),
  );

  const [activeId, setActiveId] = useState<string | null>(null);

  const ids = useMemo(
    () => items.map((_, i) => String(startIndex + i)),
    [items, startIndex],
  );

  const scrollRef = useRef<HTMLDivElement>(null);
  // Virtualize the Up Next list. Without this, dnd-kit measures every
  // row's bounding rect on the first dragmove — with 800-track queues
  // that pegs the main thread for hundreds of ms (the "freeze" the user
  // hit). Virtualization keeps the rendered rows around ~20 even on
  // huge queues, and the SortableContext below still receives the full
  // id list so dnd-kit knows the abstract ordering.
  const rowVirtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => QUEUE_ROW_HEIGHT,
    overscan: 6,
  });

  const handleDragStart = useCallback((e: DragStartEvent) => {
    setActiveId(String(e.active.id));
  }, []);

  const handleDragEnd = useCallback(
    (e: DragEndEvent) => {
      setActiveId(null);
      const { active, over } = e;
      if (!over || active.id === over.id) return;
      const from = Number(active.id);
      const to = Number(over.id);
      if (Number.isFinite(from) && Number.isFinite(to)) {
        onReorder(from, to);
      }
    },
    [onReorder],
  );

  const handleDragCancel = useCallback(() => {
    setActiveId(null);
  }, []);

  const activeIndex = activeId ? Number(activeId) - startIndex : -1;
  const activeItem =
    activeIndex >= 0 && activeIndex < items.length ? items[activeIndex] : null;

  return (
    <DndContext
      sensors={sensors}
      collisionDetection={closestCenter}
      modifiers={[restrictToVerticalAxis]}
      // Always-measure plays well with virtualization: rows entering
      // the window mid-drag get measured on the fly instead of in a
      // synchronous burst at first dragmove.
      measuring={{ droppable: { strategy: MeasuringStrategy.Always } }}
      onDragStart={handleDragStart}
      onDragEnd={handleDragEnd}
      onDragCancel={handleDragCancel}
    >
      <SortableContext items={ids} strategy={verticalListSortingStrategy}>
        <div
          ref={scrollRef}
          className="max-h-[55vh] overflow-y-auto scrollbar-hide"
        >
          <div
            style={{
              height: `${rowVirtualizer.getTotalSize()}px`,
              position: "relative",
              width: "100%",
            }}
          >
            {rowVirtualizer.getVirtualItems().map((virtualRow) => {
              const item = items[virtualRow.index];
              if (!item) return null;
              const absoluteIndex = startIndex + virtualRow.index;
              return (
                <SortableQueueRow
                  key={absoluteIndex}
                  id={String(absoluteIndex)}
                  absoluteIndex={absoluteIndex}
                  item={item}
                  top={virtualRow.start}
                  rowHeight={QUEUE_ROW_HEIGHT}
                  onJump={onJump}
                />
              );
            })}
          </div>
        </div>
      </SortableContext>
      {/* Portal the overlay to <body>: the QueuePanel itself carries a
          CSS `transform` (translate-x-…) for its slide-in animation,
          which makes it the containing block for any `position: fixed`
          descendant. Without the portal, dnd-kit's overlay (fixed) is
          positioned relative to the panel, so the dragged track sits
          off-screen and the user sees a "drag-but-no-preview" freeze.
          Rendering through `document.body` escapes the transform. */}
      {createPortal(
        <DragOverlay dropAnimation={null}>
          {activeItem ? <QueueRowPreview item={activeItem} /> : null}
        </DragOverlay>,
        document.body,
      )}
    </DndContext>
  );
}

/**
 * Stripped-down row used inside `<DragOverlay>`. Rendered detached
 * from the SortableContext, so no useSortable hook and no
 * neighbour-aware transforms — just a static visual proxy.
 */
function QueueRowPreview({ item }: { item: QueueTrackPayload }) {
  return (
    <div className="flex items-center space-x-2 p-2 rounded-lg bg-white dark:bg-zinc-800 shadow-lg border border-zinc-200 dark:border-zinc-700 select-none">
      <div className="shrink-0 p-1 -ml-1 text-zinc-400">
        <GripVertical size={14} />
      </div>
      <Artwork
        path={item.artwork_path}
        className="w-10 h-10"
        iconSize={18}
        alt={item.album_title ?? item.title}
        rounded="md"
      />
      <div className="flex-1 min-w-0">
        <div className="text-sm truncate text-zinc-800 dark:text-zinc-200">
          {item.title}
        </div>
        <div className="text-xs text-zinc-500 truncate">
          {item.artist_name ?? "—"}
        </div>
      </div>
    </div>
  );
}

/**
 * Single sortable row. The drag handle is the small left-edge grip —
 * the rest of the row stays clickable / double-clickable so the
 * existing "double-click to jump" gesture isn't broken.
 *
 * Wrapped in `memo` so the per-row re-render cost during a drag stays
 * bounded: dnd-kit's `useSortable` already handles its own internal
 * subscription, so non-dragging rows don't need to re-render when the
 * cursor moves between siblings.
 */
const SortableQueueRow = memo(function SortableQueueRow({
  id,
  absoluteIndex,
  item,
  top,
  rowHeight,
  onJump,
}: {
  id: string;
  absoluteIndex: number;
  item: QueueTrackPayload;
  top: number;
  rowHeight: number;
  onJump: (absoluteIndex: number) => void;
}) {
  // `animateLayoutChanges: () => false` disables the CSS transition
  // dnd-kit applies to every neighbour as the drag crosses them — on
  // a long queue (800+ tracks) animating that many elements in
  // parallel is what makes the cursor feel stuck.
  const { attributes, listeners, setNodeRef, transform, isDragging } = useSortable({
    id,
    animateLayoutChanges: () => false,
  });
  // Place the row's slot via CSS `top` (not via a translateY
  // transform): dnd-kit anchors the drag overlay and resolves drop
  // targets from `offsetTop`, which doesn't see CSS transforms. With
  // `transform: translateY(start)` every row reports `offsetTop = 0`
  // and the overlay snaps to viewport top + drops never land on the
  // intended target (the song appears to "snap back" because the
  // collision picks the row that's already at the same index). Using
  // CSS `top` keeps offsetTop honest. useSortable's own transform
  // stays as the only `transform` on the element.
  const sortableTransform = CSS.Transform.toString(transform);
  const style: React.CSSProperties = {
    position: "absolute",
    top: `${top}px`,
    left: 0,
    width: "100%",
    height: `${rowHeight}px`,
    transform: sortableTransform || undefined,
    // While the row is the active drag, hide it in place — the
    // `<DragOverlay>` renders the visible copy that follows the
    // cursor. Keeping the original mounted (not unmounted) preserves
    // the slot so neighbours don't shift unexpectedly.
    opacity: isDragging ? 0 : 1,
  };
  return (
    <div ref={setNodeRef} style={style}>
      <div
        onDoubleClick={() => onJump(absoluteIndex)}
        className="group flex items-center space-x-2 p-2 rounded-lg transition-colors select-none cursor-pointer hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
      >
        <button
          type="button"
          {...attributes}
          {...listeners}
          aria-label="Drag to reorder"
          className="shrink-0 p-1 -ml-1 text-zinc-300 dark:text-zinc-600 hover:text-zinc-500 dark:hover:text-zinc-400 cursor-grab active:cursor-grabbing opacity-0 group-hover:opacity-100 transition-opacity"
          onClick={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
        >
          <GripVertical size={14} />
        </button>
        <Artwork
          path={item.artwork_path}
          className="w-10 h-10"
          iconSize={18}
          alt={item.album_title ?? item.title}
          rounded="md"
        />
        <div className="flex-1 min-w-0">
          <div className="text-sm truncate text-zinc-800 dark:text-zinc-200">
            {item.title}
          </div>
          <div className="text-xs text-zinc-500 truncate">
            {item.artist_name ?? "—"}
          </div>
        </div>
      </div>
    </div>
  );
});

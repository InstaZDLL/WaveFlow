import { useCallback, useRef } from "react";
import { ArrowDown, ArrowUp } from "lucide-react";
import {
  MAX_COLUMN_WIDTH,
  specFor,
  tagKeyOf,
  type ColumnId,
  type ColumnLayout,
} from "../../../lib/trackColumns";

/** How much one arrow key moves a column edge. `Shift` multiplies it,
 *  which is the convention every slider in the app already uses. */
const KEY_STEP = 8;
const KEY_STEP_LARGE = 48;

/** The narrow slice of `t` this file needs. Mirrors the local alias in
 *  `LibraryView`, which is where this header is rendered from: importing
 *  i18next's `TFunction` instead makes the prop incompatible with the
 *  `t` the caller already has. */
type Translator = (key: string, options?: Record<string, unknown>) => string;

export interface TrackSort {
  orderBy: string;
  direction: "asc" | "desc";
}

interface TrackTableHeaderProps {
  layout: ColumnLayout;
  gridCols: string;
  sort: TrackSort | null;
  onSort: (orderBy: string) => void;
  /** Called on every pointer move and every arrow key. Renders, does
   *  not persist. */
  onPreview: (id: ColumnId, width: number) => void;
  /** Called once when the gesture ends. Persists whatever the last
   *  preview showed. */
  onCommit: (id: ColumnId) => void;
  /** Double-click on a handle: fit the column to its content. Returns
   *  nothing — the caller measures and persists. */
  onFit: (id: ColumnId) => void;
  /** Chrome on the left of the configurable columns: the index, and the
   *  artwork thumbnail when the list view shows one. */
  leadingSpacers: number;
  t: Translator;
}

/**
 * The track table's header row (#588).
 *
 * Two things it has to get right, both of which are ways a resizable
 * table normally goes wrong:
 *
 * - **The drag handle lives outside the sort button.** Inside it, a
 *   press to widen the column fires the sort on release, so every
 *   resize reorders the list.
 * - **It stays visible while scrolling.** A value with no label above
 *   it means nothing, and that goes double for a column the user chose
 *   themselves out of their own file tags. `-top-8` because the TopBar
 *   sits outside the page scroller, whose `p-8` would otherwise leave a
 *   32px band above the pinned header for rows to scroll through --
 *   the same fix as the history view's day headers. (It was `top-16`,
 *   which assumed the TopBar scrolled with the page and left a 96px gap.)
 * - **The handle is reachable from the keyboard.** A drag is not an
 *   interaction everyone can perform, and a column that can only be
 *   widened with a pointer is a column some users cannot read. Arrow
 *   keys move the edge, `Home` fits it to its content — the same thing
 *   a double-click does.
 * - **A drag is not persisted on every pointer move.** `onResize` writes
 *   through `useProfileSetting`, which serializes one database write per
 *   call — a single drag across the table would queue hundreds of them,
 *   each one a round trip. The width in flight is local state; the
 *   commit happens once, on release.
 */
export function TrackTableHeader({
  layout,
  gridCols,
  sort,
  onSort,
  onPreview,
  onCommit,
  onFit,
  leadingSpacers,
  t,
}: TrackTableHeaderProps) {
  // The drag in flight. A ref rather than state: this updates on every
  // pointer move, and re-rendering the whole header at pointer rate
  // would drop frames on a long list.
  const drag = useRef<{
    id: ColumnId;
    /** The pointer that started it. A second one -- another finger, a
     *  pen alongside a touch -- reaches the same handlers, and without
     *  this its coordinates would be measured against a gesture it has
     *  nothing to do with. */
    pointerId: number;
    startX: number;
    startWidth: number;
    /** The document was right-to-left when the gesture began. Read once,
     *  at `pointerdown`: the direction cannot change mid-drag, and
     *  asking the DOM every pointer move is a layout read per frame. */
    rtl: boolean;
  } | null>(null);

  /** Is the interface running right-to-left? Arabic, of the seventeen
   *  locales — `i18n/index.ts` stamps `dir` on the document root. */
  const isRtl = () =>
    typeof document !== "undefined" &&
    document.documentElement.getAttribute("dir") === "rtl";

  /** The column's width on screen right now.
   *
   *  A drag has to start from what the user sees, not from the spec's
   *  default: the title column is flexible, so its rendered width is
   *  whatever the grid gave it — starting the drag at 280px makes it
   *  jump the moment the pointer moves. Falls back to the declared
   *  width when the element cannot be measured. */
  const startWidthFor = (element: Element | null, fallback: number) => {
    const measured = element?.parentElement?.getBoundingClientRect().width;
    // `0` is what an element that is not laid out measures, and it is a
    // number -- so `??` alone would take it and start the gesture from
    // zero, snapping the column shut on the first pointer move.
    return measured != null && measured > 0 ? measured : fallback;
  };

  const onPointerDown = useCallback(
    (
      event: React.PointerEvent<HTMLDivElement>,
      id: ColumnId,
      width: number,
    ) => {
      // Left button only: a right-click on a handle is a context menu,
      // and a middle-click is a paste on X11.
      if (event.button !== 0) return;
      event.preventDefault();
      event.stopPropagation();
      // Already dragging: a second pointer does not take over.
      if (drag.current) return;
      drag.current = {
        id,
        pointerId: event.pointerId,
        startX: event.clientX,
        startWidth: startWidthFor(event.currentTarget, width),
        rtl: isRtl(),
      };
      // Captured on the handle, so the pointer can leave the element —
      // which it does immediately, since the column is growing out from
      // under it.
      event.currentTarget.setPointerCapture(event.pointerId);
    },
    [],
  );

  const onPointerMove = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      const current = drag.current;
      if (!current || current.pointerId !== event.pointerId) return;
      const spec = specFor(current.id);
      // Away from the column's *start* edge widens it, and which edge
      // that is depends on the direction: in RTL the handle sits on the
      // left and dragging left is what makes the column bigger.
      // Unflipped, the column shrank when the pointer said grow.
      const delta = (event.clientX - current.startX) * (current.rtl ? -1 : 1);
      const next = Math.min(
        MAX_COLUMN_WIDTH,
        Math.max(spec.minWidth, current.startWidth + delta),
      );
      // Not persisted: the parent renders this immediately, and the
      // write happens once on release.
      onPreview(current.id, next);
    },
    [onPreview],
  );

  const endDrag = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      if (!drag.current || drag.current.pointerId !== event.pointerId) return;
      const finished = drag.current;
      drag.current = null;
      if (event.currentTarget.hasPointerCapture(event.pointerId)) {
        event.currentTarget.releasePointerCapture(event.pointerId);
      }
      // The one write of the whole gesture.
      onCommit(finished.id);
    },
    [onCommit],
  );

  const onKeyDown = useCallback(
    (
      event: React.KeyboardEvent<HTMLDivElement>,
      id: ColumnId,
      width: number,
    ) => {
      const spec = specFor(id);
      // Same reason as the drag: an arrow key on a flexible column has
      // to move from where it is, not from where its default says.
      const from = startWidthFor(event.currentTarget, width);
      const step = event.shiftKey ? KEY_STEP_LARGE : KEY_STEP;
      let next: number;
      // Mirrored with the layout, like the drag: the arrow pointing
      // away from the column's start edge is the one that widens it.
      const grow = isRtl() ? "ArrowLeft" : "ArrowRight";
      const shrink = isRtl() ? "ArrowRight" : "ArrowLeft";
      switch (event.key) {
        case shrink:
          next = from - step;
          break;
        case grow:
          next = from + step;
          break;
        case "Home":
          // The keyboard's equivalent of the double-click. `Home`
          // rather than `Enter`, which a screen reader sends to
          // activate and would then mean two different things.
          event.preventDefault();
          onFit(id);
          return;
        default:
          return;
      }
      // Only once a key is actually handled: leaving this at the top
      // would swallow Tab and trap focus on the handle.
      event.preventDefault();
      const clamped = Math.min(MAX_COLUMN_WIDTH, Math.max(spec.minWidth, next));
      // One keypress is one complete gesture, so it previews and
      // commits in the same breath. Holding a key repeats it, which
      // `useProfileSetting` serializes — at key-repeat rate, not at
      // pointer rate.
      onPreview(id, clamped);
      onCommit(id);
    },
    [onCommit, onFit, onPreview],
  );

  return (
    // No `role="row"`, and no `columnheader` below it. ARIA's grid
    // roles only mean anything inside a `table` / `grid` ancestor, and
    // this is not one: the body rows are `role="button"`, because a
    // row here is a thing you activate rather than a cell you navigate.
    // An orphaned `row` makes the whole structure invalid, and the
    // `aria-sort` it carried was silently ignored -- leaving the sort
    // direction announced by nothing at all, since the arrow is
    // `aria-hidden`. It rides in the button's accessible name instead.
    <div
      className="sticky -top-8 z-20 grid gap-4 px-5 py-3 text-[10px] font-bold tracking-widest text-zinc-400 uppercase border-b border-zinc-100 dark:border-zinc-800 bg-white dark:bg-surface-dark"
      style={{ gridTemplateColumns: gridCols }}
    >
      {/* Index, and the artwork thumbnail when the list view shows one.
          Neither carries a value, so neither is a column. */}
      <span className="text-right">{t("library.table.number")}</span>
      {Array.from({ length: leadingSpacers - 1 }, (_, i) => (
        <span key={`lead-${i}`} aria-hidden="true" />
      ))}

      {layout.order.map((id) => {
        const spec = specFor(id);
        const tag = tagKeyOf(id);
        const label =
          tag !== null ? tag : t(`library.columns.${spec.labelKey}`);
        const active = sort && spec.sortKey === sort.orderBy;
        // `layout` already carries the width in flight: the parent
        // merges it in before building the grid, so the header, the
        // rows and this handle all read the same number.
        const width = layout.widths[id] ?? spec.defaultWidth;
        const justify =
          spec.align === "right"
            ? "justify-end"
            : spec.align === "center"
              ? "justify-center"
              : "justify-start";
        return (
          <div
            key={id}
            // Not decoration: `fitColumn` reads this cell's computed
            // style to measure the header label in the font it is
            // actually drawn in. It used to find it by `role`, which
            // was the wrong thing to hang a measurement on and is gone
            // now anyway.
            data-track-header={id}
            className={`relative flex items-center ${justify} min-w-0`}
          >
            {spec.sortKey ? (
              <button
                type="button"
                onClick={() => onSort(spec.sortKey as string)}
                // The label alone for an unsorted column; the label and
                // the direction for the active one. Only set when it
                // says more than the text already does, so an ordinary
                // column keeps its own content as its name.
                aria-label={
                  active
                    ? `${label} — ${t(
                        sort.direction === "asc"
                          ? "sort.ascending"
                          : "sort.descending",
                      )}`
                    : undefined
                }
                className={`flex items-center gap-1 min-w-0 rounded hover:text-zinc-600 dark:hover:text-zinc-200 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${
                  active ? "text-zinc-700 dark:text-zinc-200" : ""
                }`}
                title={label}
              >
                <span className="truncate">{label}</span>
                {active &&
                  (sort.direction === "asc" ? (
                    <ArrowUp size={11} aria-hidden="true" />
                  ) : (
                    <ArrowDown size={11} aria-hidden="true" />
                  ))}
              </button>
            ) : (
              // A custom tag has no sort: its values live in a side
              // table the listing query does not join, and a control
              // that silently does nothing is worse than none.
              <span className="truncate" title={label}>
                {label}
              </span>
            )}

            <div
              role="separator"
              aria-orientation="vertical"
              aria-label={t("library.columns.resize", { column: label })}
              tabIndex={0}
              aria-valuenow={width}
              aria-valuemin={spec.minWidth}
              aria-valuemax={MAX_COLUMN_WIDTH}
              onKeyDown={(event) => onKeyDown(event, id, width)}
              onPointerDown={(event) => onPointerDown(event, id, width)}
              onPointerMove={onPointerMove}
              onPointerUp={endDrag}
              onPointerCancel={endDrag}
              // Capture can be lost without either of the two above:
              // the browser takes it back for a system gesture, or the
              // handle re-renders under the pointer. `drag.current`
              // would survive, and the next pointermove -- with no
              // pointerdown before it -- would resize from a start
              // position taken minutes ago. `endDrag` returns early
              // when there is no drag, so the ordinary release path
              // reaching this second is a no-op.
              onLostPointerCapture={endDrag}
              onDoubleClick={(event) => {
                event.preventDefault();
                event.stopPropagation();
                onFit(id);
              }}
              className="absolute -end-2 top-0 bottom-0 w-3 cursor-col-resize touch-none focus:outline-none focus-visible:after:bg-emerald-500 after:content-[''] after:absolute after:inset-y-1 after:left-1/2 after:w-px after:bg-transparent hover:after:bg-emerald-500"
            />
          </div>
        );
      })}

      {/* Like, and the overflow menu. */}
      <span aria-hidden="true" />
      <span aria-hidden="true" />
    </div>
  );
}

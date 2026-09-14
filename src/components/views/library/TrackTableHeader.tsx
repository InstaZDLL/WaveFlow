import { useCallback, useRef } from "react";
import { ArrowDown, ArrowUp } from "lucide-react";
import {
  specFor,
  tagKeyOf,
  type ColumnId,
  type ColumnLayout,
} from "../../../lib/trackColumns";

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
  onResize: (id: ColumnId, width: number) => void;
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
 *   themselves out of their own file tags. `top-16` because the TopBar
 *   is `sticky top-0 h-16` and owns the space above.
 */
export function TrackTableHeader({
  layout,
  gridCols,
  sort,
  onSort,
  onResize,
  onFit,
  leadingSpacers,
  t,
}: TrackTableHeaderProps) {
  // The drag in flight. A ref rather than state: this updates on every
  // pointer move, and re-rendering the whole header at pointer rate
  // would drop frames on a long list.
  const drag = useRef<{ id: ColumnId; startX: number; startWidth: number } | null>(
    null,
  );

  const onPointerDown = useCallback(
    (event: React.PointerEvent<HTMLDivElement>, id: ColumnId, width: number) => {
      // Left button only: a right-click on a handle is a context menu,
      // and a middle-click is a paste on X11.
      if (event.button !== 0) return;
      event.preventDefault();
      event.stopPropagation();
      drag.current = { id, startX: event.clientX, startWidth: width };
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
      if (!current) return;
      const spec = specFor(current.id);
      const next = Math.max(
        spec.minWidth,
        current.startWidth + (event.clientX - current.startX),
      );
      onResize(current.id, next);
    },
    [onResize],
  );

  const endDrag = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      if (!drag.current) return;
      drag.current = null;
      if (event.currentTarget.hasPointerCapture(event.pointerId)) {
        event.currentTarget.releasePointerCapture(event.pointerId);
      }
    },
    [],
  );

  return (
    <div
      role="row"
      className="sticky top-16 z-20 grid gap-4 px-5 py-3 text-[10px] font-bold tracking-widest text-zinc-400 uppercase border-b border-zinc-100 dark:border-zinc-800 bg-white dark:bg-surface-dark"
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
        const label = tag !== null ? tag : t(`library.columns.${spec.labelKey}`);
        const active = sort && spec.sortKey === sort.orderBy;
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
            role="columnheader"
            aria-sort={
              active
                ? sort.direction === "asc"
                  ? "ascending"
                  : "descending"
                : undefined
            }
            className={`relative flex items-center ${justify} min-w-0`}
          >
            {spec.sortKey ? (
              <button
                type="button"
                onClick={() => onSort(spec.sortKey as string)}
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
              onPointerDown={(event) => onPointerDown(event, id, width)}
              onPointerMove={onPointerMove}
              onPointerUp={endDrag}
              onPointerCancel={endDrag}
              onDoubleClick={(event) => {
                event.preventDefault();
                event.stopPropagation();
                onFit(id);
              }}
              className="absolute -right-2 top-0 bottom-0 w-3 cursor-col-resize touch-none after:content-[''] after:absolute after:inset-y-1 after:left-1/2 after:w-px after:bg-transparent hover:after:bg-emerald-500"
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

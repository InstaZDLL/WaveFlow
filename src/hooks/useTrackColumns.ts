import { useCallback, useMemo } from "react";
import { useProfileSetting } from "./useProfileSetting";
import {
  DEFAULT_LAYOUT,
  MAX_COLUMN_WIDTH,
  sanitizeLayout,
  specFor,
  type ColumnId,
  type ColumnLayout,
} from "../lib/trackColumns";

const KEY = "ui.track_columns";

/** Broadcast after a successful write so every mounted track list
 *  re-reads together — the library, the folder browser and the
 *  inventory all render the same table. */
export const TRACK_COLUMNS_EVENT = "waveflow:track-columns";

const parse = (raw: string | null): ColumnLayout => {
  if (!raw) return DEFAULT_LAYOUT;
  try {
    return sanitizeLayout(JSON.parse(raw) as ColumnLayout);
  } catch {
    // A row written by a future version, or a hand-edited one. The
    // default is a working table; a throw here would be a blank one.
    return DEFAULT_LAYOUT;
  }
};

const serialize = (value: ColumnLayout) => JSON.stringify(value);

export interface TrackColumns {
  layout: ColumnLayout;
  /** Move the column at `from` to `to`, resolved against the stored
   *  order rather than the one the caller last rendered. */
  move: (from: number, to: number) => Promise<void>;
  /** `false` until the stored choice has been read for the active
   *  profile. The table waits on it: painting the default set first and
   *  the stored one a moment later moves every column under the
   *  pointer. */
  ready: boolean;
  toggle: (id: ColumnId) => Promise<void>;
  setWidth: (id: ColumnId, width: number) => Promise<void>;
  /** Put every width back to its default, keeping the chosen columns. */
  resetWidths: () => Promise<void>;
  reset: () => Promise<void>;
}

/**
 * Per-profile column choice, order and widths for the track table
 * (#588).
 *
 * Concurrency, profile isolation and rollback live in
 * [`useProfileSetting`](./useProfileSetting.ts), per the settings
 * invariant — this adds only the shape of the value and the four edits
 * the UI makes to it.
 */
export function useTrackColumns(): TrackColumns {
  const { value, ready, setValue } = useProfileSetting<ColumnLayout>({
    key: KEY,
    defaultValue: DEFAULT_LAYOUT,
    parse,
    serialize,
    valueType: "json",
    event: TRACK_COLUMNS_EVENT,
    label: "useTrackColumns",
  });

  const layout = useMemo(() => sanitizeLayout(value), [value]);

  // Every action below writes through a functional update rather than
  // the `layout` this render captured. `useProfileSetting` serialises
  // writes precisely so each one can start from the result of the last,
  // and the column picker is where that matters: ticking three boxes in
  // quick succession queues three writes, and off a render-lagged
  // snapshot the last would land carrying none of the other two.
  // Indices, not the reordered array: computing the array needs an
  // order to splice, and a caller can only splice the one its render
  // captured -- which puts the snapshot back that the functional update
  // exists to avoid. Two quick drags would then land the second on a
  // list that never had the first.
  const move = useCallback(
    (from: number, to: number) =>
      setValue((previous) => {
        const base = sanitizeLayout(previous);
        if (from === to || from < 0 || from >= base.order.length) return base;
        if (to < 0 || to >= base.order.length) return base;
        const order = [...base.order];
        const [moved] = order.splice(from, 1);
        order.splice(to, 0, moved);
        return sanitizeLayout({ ...base, order });
      }),
    [setValue],
  );

  const toggle = useCallback(
    (id: ColumnId) =>
      setValue((previous) => {
        // The add-or-remove decision is read from `previous` too, not
        // just the list it edits: judged against a stale snapshot, two
        // quick toggles of the same column would both decide "add".
        const base = sanitizeLayout(previous);
        const present = base.order.includes(id);
        // Removing the title is reachable by unticking one box and
        // leaves a table of metadata about songs it does not name, so
        // `sanitizeLayout` puts it back — silently, because the
        // checkbox for it is disabled and this path is only reachable
        // through a stored value.
        const order = present
          ? base.order.filter((other) => other !== id)
          : [...base.order, id];
        return sanitizeLayout({ ...base, order });
      }),
    [setValue],
  );

  const setWidth = useCallback(
    (id: ColumnId, width: number) =>
      setValue((previous) => {
        const base = sanitizeLayout(previous);
        return {
          ...base,
          widths: {
            ...base.widths,
            // Both bounds, like the header's drag and arrow keys. This
            // is the single place a width is persisted, so a caller that
            // clamps only one of them cannot store something the table
            // will refuse to render.
            [id]: Math.min(
              MAX_COLUMN_WIDTH,
              Math.max(specFor(id).minWidth, Math.round(width)),
            ),
          },
        };
      }),
    [setValue],
  );

  const resetWidths = useCallback(
    () => setValue((previous) => ({ ...sanitizeLayout(previous), widths: {} })),
    [setValue],
  );

  const reset = useCallback(() => setValue(DEFAULT_LAYOUT), [setValue]);

  return { layout, ready, move, toggle, setWidth, resetWidths, reset };
}

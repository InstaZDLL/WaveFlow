import { useCallback, useMemo } from "react";
import { useProfileSetting } from "./useProfileSetting";
import {
  DEFAULT_LAYOUT,
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
  /** `false` until the stored choice has been read for the active
   *  profile. The table waits on it: painting the default set first and
   *  the stored one a moment later moves every column under the
   *  pointer. */
  ready: boolean;
  setOrder: (order: ColumnId[]) => Promise<void>;
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

  const setOrder = useCallback(
    (order: ColumnId[]) => setValue(sanitizeLayout({ ...layout, order })),
    [layout, setValue],
  );

  const toggle = useCallback(
    (id: ColumnId) => {
      const present = layout.order.includes(id);
      // Removing the title is reachable by unticking one box and leaves
      // a table of metadata about songs it does not name, so
      // `sanitizeLayout` puts it back — silently, because the checkbox
      // for it is disabled and this path is only reachable through a
      // stored value.
      const order = present
        ? layout.order.filter((other) => other !== id)
        : [...layout.order, id];
      return setValue(sanitizeLayout({ ...layout, order }));
    },
    [layout, setValue],
  );

  const setWidth = useCallback(
    (id: ColumnId, width: number) =>
      setValue({
        ...layout,
        widths: {
          ...layout.widths,
          [id]: Math.max(specFor(id).minWidth, Math.round(width)),
        },
      }),
    [layout, setValue],
  );

  const resetWidths = useCallback(
    () => setValue({ ...layout, widths: {} }),
    [layout, setValue],
  );

  const reset = useCallback(() => setValue(DEFAULT_LAYOUT), [setValue]);

  return { layout, ready, setOrder, toggle, setWidth, resetWidths, reset };
}

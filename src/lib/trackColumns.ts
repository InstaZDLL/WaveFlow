import type { LibraryTrackRow } from "./tauri/browse";

/**
 * Which columns a track list shows, in what order, and how wide (#588).
 *
 * Track lists shipped one fixed set of columns, so somebody whose
 * library is organised around a field we don't show — a composer, a
 * catalogue number, a rip source kept in a custom tag — had no way to
 * see it and no way to sort by it.
 *
 * # What is a column and what is not
 *
 * The row's chrome — the index / playing indicator, the artwork
 * thumbnail, the like heart, the overflow menu — is **not** in this
 * list. None of it carries a value, none of it sorts, and hiding the
 * overflow menu would remove the only way to reach half the actions. It
 * stays pinned at the two ends and the configurable columns sit
 * between.
 *
 * # Custom tags
 *
 * An id of the form `tag:<key>` reads a frame the scanner found in the
 * user's own files. Those are offered from what the library actually
 * holds, with a count per key, rather than from the list of frames the
 * formats theoretically allow — which would bury the five useful ones
 * under thirty-five nobody has.
 */

/** A column backed by a field the library already holds. */
export type BuiltinColumnId =
  | "title"
  | "artist"
  | "album"
  | "year"
  | "track_number"
  | "disc_number"
  | "duration_ms"
  | "rating"
  | "bitrate"
  | "sample_rate"
  | "bit_depth"
  | "channels"
  | "codec"
  | "musical_key"
  | "file_size"
  | "added_at"
  | "file_path";

export type ColumnId = BuiltinColumnId | `tag:${string}`;

export interface ColumnSpec {
  id: ColumnId;
  /** i18n key under `library.columns.`. */
  labelKey: string;
  /** Starting width in pixels. */
  defaultWidth: number;
  /** Below this the content is unreadable rather than merely tight. */
  minWidth: number;
  /** The `order_by` value the backend understands, when it sorts. */
  sortKey?: string;
  align?: "left" | "right" | "center";
  /**
   * Takes the leftover space instead of a fixed width.
   *
   * Only the title does. It is the row's identity, so it must be the
   * one that grows — and it carries a floor, because if it is the only
   * flexible column it absorbs every pixel the others take and
   * disappears the moment a few columns are added.
   */
  flexible?: boolean;
}

export const BUILTIN_COLUMNS: Record<BuiltinColumnId, ColumnSpec> = {
  title: {
    id: "title",
    labelKey: "title",
    defaultWidth: 280,
    minWidth: 120,
    sortKey: "title",
    flexible: true,
  },
  artist: {
    id: "artist",
    labelKey: "artist",
    defaultWidth: 200,
    minWidth: 80,
    sortKey: "artist",
  },
  album: {
    id: "album",
    labelKey: "album",
    defaultWidth: 200,
    minWidth: 80,
    sortKey: "album",
  },
  year: {
    id: "year",
    labelKey: "year",
    defaultWidth: 72,
    minWidth: 56,
    sortKey: "year",
    align: "right",
  },
  track_number: {
    id: "track_number",
    labelKey: "trackNumber",
    defaultWidth: 72,
    minWidth: 56,
    sortKey: "track_number",
    align: "right",
  },
  disc_number: {
    id: "disc_number",
    labelKey: "discNumber",
    defaultWidth: 72,
    minWidth: 56,
    sortKey: "disc_number",
    align: "right",
  },
  duration_ms: {
    id: "duration_ms",
    labelKey: "duration",
    defaultWidth: 80,
    minWidth: 64,
    sortKey: "duration_ms",
    align: "right",
  },
  rating: {
    id: "rating",
    labelKey: "rating",
    defaultWidth: 112,
    minWidth: 96,
    sortKey: "rating",
  },
  bitrate: {
    id: "bitrate",
    labelKey: "bitrate",
    defaultWidth: 96,
    minWidth: 72,
    sortKey: "bitrate",
    align: "right",
  },
  sample_rate: {
    id: "sample_rate",
    labelKey: "sampleRate",
    defaultWidth: 104,
    minWidth: 72,
    sortKey: "sample_rate",
    align: "right",
  },
  bit_depth: {
    id: "bit_depth",
    labelKey: "bitDepth",
    defaultWidth: 88,
    minWidth: 64,
    sortKey: "bit_depth",
    align: "right",
  },
  channels: {
    id: "channels",
    labelKey: "channels",
    defaultWidth: 88,
    minWidth: 64,
    sortKey: "channels",
    align: "right",
  },
  codec: {
    id: "codec",
    labelKey: "codec",
    defaultWidth: 96,
    minWidth: 64,
    sortKey: "codec",
  },
  musical_key: {
    id: "musical_key",
    labelKey: "musicalKey",
    defaultWidth: 88,
    minWidth: 64,
    sortKey: "musical_key",
  },
  file_size: {
    id: "file_size",
    labelKey: "fileSize",
    defaultWidth: 96,
    minWidth: 72,
    sortKey: "file_size",
    align: "right",
  },
  added_at: {
    id: "added_at",
    labelKey: "addedAt",
    defaultWidth: 128,
    minWidth: 96,
    sortKey: "added_at",
    align: "right",
  },
  file_path: {
    id: "file_path",
    labelKey: "filePath",
    defaultWidth: 320,
    minWidth: 120,
    sortKey: "file_path",
  },
};

/**
 * What a profile sees before it has chosen anything — and what a
 * profile written before #588 keeps seeing, which is the point: the
 * stored preference is absent for every existing profile, and a missing
 * preference must not mean "no columns".
 */
export const DEFAULT_COLUMNS: ColumnId[] = [
  "title",
  "artist",
  "album",
  "rating",
  "duration_ms",
];

/**
 * The widest a column may be made.
 *
 * Shared by the drag, the keyboard and the fit so all three agree —
 * and so the handle can honestly declare an `aria-valuemax`. Without a
 * ceiling, one column can be dragged wide enough to push every other
 * one out of the viewport, with no way back except the reset.
 */
export const MAX_COLUMN_WIDTH = 640;

/** Widths are stored per column id, so hiding and re-showing a column
 *  brings back the width the user gave it. */
export interface ColumnLayout {
  order: ColumnId[];
  widths: Partial<Record<string, number>>;
}

export const DEFAULT_LAYOUT: ColumnLayout = {
  order: DEFAULT_COLUMNS,
  widths: {},
};

/**
 * A custom tag column's key, or `null` for a built-in one.
 *
 * A bare `"tag:"` is not a tag column: it would render a header with no
 * label and a cell that can never match a key, so it reads as `null`
 * and `sanitizeLayout` drops it.
 */
export function tagKeyOf(id: ColumnId): string | null {
  if (!id.startsWith("tag:")) return null;
  const key = id.slice(4);
  return key.length > 0 ? key : null;
}

/**
 * The spec for any id, built on the fly for custom tags.
 *
 * A tag column is never flexible and never sortable: its values live in
 * a side table the listing query does not join, so offering a sort that
 * the backend would silently ignore is worse than offering none.
 */
const hasOwn = (target: object, key: string) =>
  Object.prototype.hasOwnProperty.call(target, key);

export function specFor(id: ColumnId): ColumnSpec {
  const tag = tagKeyOf(id);
  if (tag !== null) {
    return {
      id,
      labelKey: `tag:${tag}`,
      defaultWidth: 160,
      minWidth: 72,
    };
  }
  // An own-property test, not `in` and not a bare lookup: the ids come
  // from a stored JSON preference, and `"toString"` reaches
  // `Object.prototype` — which answers with a *function*, so `??` never
  // fires and the caller goes on to read `undefined` off
  // `spec.minWidth`. Spelt through `call` rather than `Object.hasOwn`,
  // which is ES2022 and the project targets ES2020.
  return hasOwn(BUILTIN_COLUMNS, id)
    ? BUILTIN_COLUMNS[id as BuiltinColumnId]
    : BUILTIN_COLUMNS.title;
}

/**
 * Drop built-in ids we no longer know about, and guarantee the title
 * survives.
 *
 * Both halves matter for a stored preference. A built-in id can
 * disappear between versions, and rendering one paints an empty band
 * nothing explains. And a layout with no title is a table of metadata
 * about songs it does not name, which is reachable by unticking one
 * box.
 *
 * A `tag:` id is deliberately **kept** even when no track carries that
 * key any more, and that is not the same case. The key came from the
 * user's own files: a column that empties out is telling them the tag
 * is gone from their library, which is worth knowing and is a thing
 * they can put back. Dropping it would hide that, and dropping it
 * *persistently* — the layout is written back — would destroy the
 * column set on any read that came back empty for an unrelated reason.
 * Removing one is a click in the picker.
 */
export function sanitizeLayout(layout: ColumnLayout | null): ColumnLayout {
  if (!layout || !Array.isArray(layout.order)) return DEFAULT_LAYOUT;
  const seen = new Set<string>();
  const order = layout.order.filter((id) => {
    if (typeof id !== "string" || seen.has(id)) return false;
    seen.add(id);
    return tagKeyOf(id) !== null || hasOwn(BUILTIN_COLUMNS, id);
  });
  if (!order.includes("title")) order.unshift("title");
  // Widths are validated here or nowhere. `trackSizeFor` happens to
  // reject a non-number on its own, but the header reads
  // `widths[id] ?? defaultWidth` -- and `??` takes a `NaN`, which then
  // becomes the width a drag starts from. A stored preference is
  // whatever the last version, or a hand-edited row, left behind.
  const widths: Record<string, number> = {};
  const stored = layout.widths;
  if (stored && typeof stored === "object") {
    // Every recognised entry, not only the visible ones. A width
    // belongs to a column, not to the column being on screen: keyed on
    // `order`, unticking a column in the picker would silently throw
    // its width away and ticking it back would return it at the
    // default, which is a stored preference quietly losing what the
    // user set.
    for (const [rawId, value] of Object.entries(
      stored as Record<string, unknown>,
    )) {
      if (typeof value !== "number" || !Number.isFinite(value)) continue;
      const id = rawId as ColumnId;
      if (tagKeyOf(id) === null && !hasOwn(BUILTIN_COLUMNS, id)) continue;
      const spec = specFor(id);
      widths[id] = Math.min(
        MAX_COLUMN_WIDTH,
        Math.max(spec.minWidth, Math.round(value)),
      );
    }
  }
  return { order, widths };
}

/** The CSS `grid-template-columns` track for one column. */
export function trackSizeFor(id: ColumnId, layout: ColumnLayout): string {
  const spec = specFor(id);
  const stored = layout.widths[id];
  if (typeof stored === "number" && stored > 0) {
    return `${Math.min(MAX_COLUMN_WIDTH, Math.max(stored, spec.minWidth))}px`;
  }
  return spec.flexible
    ? `minmax(${spec.minWidth}px, 1fr)`
    : `${spec.defaultWidth}px`;
}

/**
 * The narrowest one column's track can get, in px: the floor of what
 * {@link trackSizeFor} returns. Summed into the table's `min-width`, so a
 * column list wider than the view widens the table itself instead of
 * spilling out of the box that paints its background and header.
 */
export function trackMinWidthFor(id: ColumnId, layout: ColumnLayout): number {
  const spec = specFor(id);
  const stored = layout.widths[id];
  if (typeof stored === "number" && stored > 0) {
    return Math.min(MAX_COLUMN_WIDTH, Math.max(stored, spec.minWidth));
  }
  return spec.flexible ? spec.minWidth : spec.defaultWidth;
}

/**
 * The layout with only the columns that fit in `availablePx` (#669).
 *
 * Columns are dropped from the right end of the user's order, so the
 * order they chose is also the priority: what they placed last goes
 * first. The title is never dropped — it is the row's identity — and
 * the chrome at both ends (`fixedPx`: index, thumbnail, heart, actions,
 * padding, border) is always counted.
 *
 * The saved layout is not touched: this only decides what one render
 * shows, so a column hidden for lack of room is back as soon as there is
 * room, in the same place and at the same width. When even the title
 * alone does not fit, the table keeps its floor and the page scrolls.
 *
 * `gapPx` is the space between two tracks; a table of n tracks has n - 1.
 */
export function fitLayoutToWidth(
  layout: ColumnLayout,
  availablePx: number,
  fixedPx: number,
  fixedTracks: number,
  gapPx: number,
): ColumnLayout {
  const order = [...layout.order];
  const needed = () =>
    fixedPx +
    order.reduce((sum, id) => sum + trackMinWidthFor(id, layout), 0) +
    gapPx * Math.max(0, fixedTracks + order.length - 1);
  while (needed() > availablePx) {
    let drop = order.length - 1;
    while (drop >= 0 && order[drop] === "title") drop -= 1;
    if (drop < 0) break;
    order.splice(drop, 1);
  }
  return order.length === layout.order.length ? layout : { ...layout, order };
}

/**
 * The plain text a column shows for a row.
 *
 * Used for the cells that are just text, and — the reason it is a
 * separate function — for measuring a column's natural width off-DOM.
 * Rows are virtualised, so the ones outside the viewport have no
 * computed layout and measuring the DOM would size the column to
 * whatever happens to be on screen.
 */
export function cellText(
  id: ColumnId,
  row: LibraryTrackRow,
  fmt: {
    duration: (ms: number) => string;
    bytes: (n: number) => string;
    /** `track.added_at`, which the backend writes in milliseconds. */
    date: (epochMs: number) => string;
    tag: (row: LibraryTrackRow, key: string) => string | null;
  },
): string {
  const tag = tagKeyOf(id);
  if (tag !== null) return fmt.tag(row, tag) ?? "";
  switch (id) {
    case "title":
      return row.title;
    case "artist":
      return row.artist_name ?? "";
    case "album":
      return row.album_title ?? "";
    case "year":
      return row.year ? String(row.year) : "";
    case "track_number":
      return row.track_number != null ? String(row.track_number) : "";
    case "disc_number":
      return row.disc_number != null ? String(row.disc_number) : "";
    case "duration_ms":
      return fmt.duration(row.duration_ms);
    case "bitrate":
      return row.bitrate != null ? `${row.bitrate} kbps` : "";
    case "sample_rate":
      return row.sample_rate != null
        ? `${(row.sample_rate / 1000).toFixed(1)} kHz`
        : "";
    case "bit_depth":
      return row.bit_depth != null ? `${row.bit_depth} bit` : "";
    case "channels":
      return row.channels != null ? String(row.channels) : "";
    case "codec":
      return row.codec ?? "";
    case "musical_key":
      return row.musical_key ?? "";
    case "file_size":
      return row.file_size != null ? fmt.bytes(row.file_size) : "";
    case "added_at":
      return fmt.date(row.added_at);
    case "file_path":
      return row.file_path ?? "";
    // Rating is stars, not text. It has no natural text width, so
    // fitting it measures the header alone — which is what its fixed
    // default already is.
    case "rating":
      return "";
    default:
      return "";
  }
}

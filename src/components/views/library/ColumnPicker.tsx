import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  ChevronDown,
  ChevronUp,
  Columns3,
  GripVertical,
  RotateCcw,
} from "lucide-react";
import {
  BUILTIN_COLUMNS,
  specFor,
  tagKeyOf,
  type BuiltinColumnId,
  type ColumnId,
  type ColumnLayout,
} from "../../../lib/trackColumns";

/** See the note in `TrackTableHeader`. */
type Translator = (key: string, options?: Record<string, unknown>) => string;

interface ColumnPickerProps {
  layout: ColumnLayout;
  /** Tag keys the library actually holds, with how many tracks carry
   *  each. Offering the theoretical list instead would bury the five
   *  useful ones under thirty-five nobody has. */
  tagKeys: { key: string; count: number }[];
  onToggle: (id: ColumnId) => void;
  onReorder: (order: ColumnId[]) => void;
  onResetWidths: () => void;
  t: Translator;
}

const BUILTIN_ORDER = Object.keys(BUILTIN_COLUMNS) as BuiltinColumnId[];

/**
 * Choose which columns show, and in what order (#588).
 *
 * Portalled, like every other overlay at this layer: the library header
 * sits under chrome carrying `backdrop-filter`, which caps a stacking
 * context and would clamp this popover behind the content it opens
 * over.
 */
export function ColumnPicker({
  layout,
  tagKeys,
  onToggle,
  onReorder,
  onResetWidths,
  t,
}: ColumnPickerProps) {
  const [open, setOpen] = useState(false);
  const [anchor, setAnchor] = useState<{ top: number; right: number } | null>(
    null,
  );
  const buttonRef = useRef<HTMLButtonElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const dragFrom = useRef<number | null>(null);

  /** Close, and put focus back where it came from.
   *
   *  For the closes the user did not aim: Escape, and a scroll or
   *  resize that moves the anchor out from under the popover. Without
   *  it, focus is sitting inside a subtree that just unmounted and
   *  falls to the document body, leaving a keyboard user to tab from
   *  the top of the page to get back.
   *
   *  A click outside is the exception and closes directly: the user
   *  aimed at something, and pulling focus back to the button would
   *  take it off whatever they just clicked. */
  const close = useCallback(() => {
    setOpen(false);
    buttonRef.current?.focus();
  }, []);

  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    // The popover is `fixed` and anchored to where the button was when
    // it opened. Scrolling the page or resizing the window moves the
    // button and leaves the popover behind, floating over unrelated
    // content. Closing is the honest answer -- re-anchoring on every
    // scroll frame would have it chase the button up the page, which is
    // worse. `capture` so a scroll inside any container is seen, and
    // the popover's own scrolling is excluded by the target check.
    const onReflow = (event: Event) => {
      if (
        event.target instanceof Element &&
        event.target.closest?.("[data-column-picker]")
      ) {
        return;
      }
      close();
    };
    const onClick = (event: MouseEvent) => {
      if (!(event.target instanceof Node)) return;
      if (buttonRef.current?.contains(event.target)) return;
      // The popover marks itself, so a click inside it does not close
      // it — a checkbox list the user ticks one box in and loses is not
      // a list anyone finishes configuring.
      if ((event.target as Element).closest?.("[data-column-picker]")) return;
      setOpen(false);
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onClick);
    document.addEventListener("scroll", onReflow, true);
    window.addEventListener("resize", onReflow);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("mousedown", onClick);
      document.removeEventListener("scroll", onReflow, true);
      window.removeEventListener("resize", onReflow);
    };
  }, [open, close]);

  // Entering the dialog on open. Without it the picker is unreachable
  // from the keyboard: the button opens a portalled panel that sits at
  // the end of the document, so Tab would walk the whole page first.
  useEffect(() => {
    if (!open) return;
    const first = dialogRef.current?.querySelector<HTMLElement>(
      "input, button, [tabindex]:not([tabindex='-1'])",
    );
    (first ?? dialogRef.current)?.focus();
  }, [open]);

  const toggleOpen = () => {
    const rect = buttonRef.current?.getBoundingClientRect();
    if (rect) {
      setAnchor({
        top: rect.bottom + 8,
        right: Math.max(8, window.innerWidth - rect.right),
      });
    }
    setOpen((value) => !value);
  };

  const chosen = new Set(layout.order);

  const move = (from: number, to: number) => {
    if (from === to || to < 0 || to >= layout.order.length) return;
    const next = [...layout.order];
    const [moved] = next.splice(from, 1);
    next.splice(to, 0, moved);
    onReorder(next);
  };

  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        onClick={toggleOpen}
        aria-expanded={open}
        aria-label={t("library.columns.choose")}
        title={t("library.columns.choose")}
        className="p-1.5 rounded-md text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
      >
        <Columns3 size={18} />
      </button>

      {open &&
        anchor &&
        createPortal(
          <div
            ref={dialogRef}
            data-column-picker
            role="dialog"
            aria-modal="false"
            tabIndex={-1}
            aria-label={t("library.columns.choose")}
            style={{ top: anchor.top, right: anchor.right }}
            className="fixed z-100 w-72 max-h-[70vh] overflow-y-auto rounded-xl border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 shadow-2xl p-3 space-y-3"
          >
            <div className="flex items-center justify-between">
              <span className="text-xs font-semibold text-zinc-700 dark:text-zinc-200">
                {t("library.columns.shown")}
              </span>
              <button
                type="button"
                onClick={onResetWidths}
                className="flex items-center gap-1 text-[11px] text-zinc-500 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-zinc-100 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 rounded"
              >
                <RotateCcw size={11} aria-hidden="true" />
                {t("library.columns.resetWidths")}
              </button>
            </div>

            {/* The chosen columns, in order, draggable. Native HTML5
                drag rather than dnd-kit: the list is a handful of rows
                inside a popover, and pulling in the drag context the
                queue uses would cost more than it buys here. */}
            <ul className="space-y-0.5">
              {layout.order.map((id, index) => {
                const tag = tagKeyOf(id);
                const label =
                  tag !== null
                    ? tag
                    : t(`library.columns.${specFor(id).labelKey}`);
                return (
                  <li
                    key={id}
                    draggable
                    onDragStart={() => {
                      dragFrom.current = index;
                    }}
                    // Fires on every end, including a cancel and a drop
                    // outside the list. Without it the index survives,
                    // and the *next* drop -- possibly on a list that has
                    // changed since -- moves whatever now sits there.
                    onDragEnd={() => {
                      dragFrom.current = null;
                    }}
                    onDragOver={(event) => event.preventDefault()}
                    onDrop={() => {
                      if (dragFrom.current !== null) {
                        move(dragFrom.current, index);
                      }
                      dragFrom.current = null;
                    }}
                    className="flex items-center gap-2 px-2 py-1.5 rounded-lg hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
                  >
                    <GripVertical
                      size={13}
                      className="shrink-0 text-zinc-400 cursor-grab"
                      aria-hidden="true"
                    />
                    <input
                      type="checkbox"
                      checked
                      // The title is the row's identity: a table of
                      // metadata about songs it does not name is
                      // reachable by unticking one box.
                      disabled={id === "title"}
                      onChange={() => onToggle(id)}
                      aria-label={label}
                      className="w-3.5 h-3.5 accent-emerald-500 shrink-0 disabled:opacity-40"
                    />
                    <span className="text-sm text-zinc-800 dark:text-zinc-200 truncate">
                      {label}
                    </span>
                    {/* Reordering has to be reachable without a drag.
                        A drag is a gesture not everyone can perform,
                        and it is the only way to change the order --
                        so without these the feature is closed to
                        keyboard and switch users entirely. */}
                    <span className="ml-auto flex shrink-0 items-center">
                      <button
                        type="button"
                        onClick={() => move(index, index - 1)}
                        disabled={index === 0}
                        aria-label={t("library.columns.moveUp", {
                          column: label,
                        })}
                        className="p-0.5 rounded text-zinc-400 hover:text-zinc-800 disabled:opacity-30 dark:hover:text-zinc-100 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
                      >
                        <ChevronUp size={13} aria-hidden="true" />
                      </button>
                      <button
                        type="button"
                        onClick={() => move(index, index + 1)}
                        disabled={index === layout.order.length - 1}
                        aria-label={t("library.columns.moveDown", {
                          column: label,
                        })}
                        className="p-0.5 rounded text-zinc-400 hover:text-zinc-800 disabled:opacity-30 dark:hover:text-zinc-100 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
                      >
                        <ChevronDown size={13} aria-hidden="true" />
                      </button>
                    </span>
                  </li>
                );
              })}
            </ul>

            <div className="pt-2 border-t border-zinc-100 dark:border-zinc-800">
              <span className="block text-xs font-semibold text-zinc-700 dark:text-zinc-200 mb-1">
                {t("library.columns.available")}
              </span>
              <ul className="space-y-0.5">
                {BUILTIN_ORDER.filter((id) => !chosen.has(id)).map((id) => (
                  <li key={id}>
                    <label className="flex items-center gap-2 px-2 py-1.5 rounded-lg hover:bg-zinc-50 dark:hover:bg-zinc-800/60 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={false}
                        onChange={() => onToggle(id)}
                        className="w-3.5 h-3.5 accent-emerald-500 shrink-0"
                      />
                      <span className="text-sm text-zinc-600 dark:text-zinc-300 truncate">
                        {t(`library.columns.${BUILTIN_COLUMNS[id].labelKey}`)}
                      </span>
                    </label>
                  </li>
                ))}
              </ul>
            </div>

            {tagKeys.length > 0 && (
              <div className="pt-2 border-t border-zinc-100 dark:border-zinc-800">
                <span className="block text-xs font-semibold text-zinc-700 dark:text-zinc-200 mb-1">
                  {t("library.columns.fromYourFiles")}
                </span>
                <ul className="space-y-0.5">
                  {tagKeys
                    .filter(({ key }) => !chosen.has(`tag:${key}`))
                    .map(({ key, count }) => (
                      <li key={key}>
                        <label className="flex items-center gap-2 px-2 py-1.5 rounded-lg hover:bg-zinc-50 dark:hover:bg-zinc-800/60 cursor-pointer">
                          <input
                            type="checkbox"
                            checked={false}
                            onChange={() => onToggle(`tag:${key}`)}
                            className="w-3.5 h-3.5 accent-emerald-500 shrink-0"
                          />
                          <span className="text-sm text-zinc-600 dark:text-zinc-300 truncate">
                            {key}
                          </span>
                          {/* The count is what makes this list usable:
                              it separates the tag on every track from
                              the one on three. */}
                          <span className="ml-auto shrink-0 text-[11px] text-zinc-400 tabular-nums">
                            {count}
                          </span>
                        </label>
                      </li>
                    ))}
                </ul>
              </div>
            )}
          </div>,
          document.body,
        )}
    </>
  );
}

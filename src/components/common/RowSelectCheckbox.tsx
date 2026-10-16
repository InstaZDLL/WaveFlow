import type { KeyboardEvent, MouseEvent, ReactNode } from "react";
import { Check } from "lucide-react";
import { useTranslation } from "react-i18next";

interface RowSelectCheckboxProps {
  /** Whether the row is in the selection. */
  checked: boolean;
  /**
   * Whether the list is in checkbox mode (`useMultiSelect().checkboxMode`).
   * While it is, every row shows its box, so adding to the selection
   * needs no modifier. Outside it the box stays hidden, hover included:
   * a selection starts with Ctrl or Shift + click, or Ctrl+A (#867).
   */
  selectionActive: boolean;
  /** Same as a Ctrl+click on the row. */
  onToggle: () => void;
  /** The row's title, so each box has a name of its own. */
  title: string;
  /** What the cell shows otherwise: the row number or the playing bars. */
  children: ReactNode;
}

/**
 * The leading cell of a selectable track row: its number, giving way to a
 * checkbox while the list is in checkbox mode.
 *
 * Not on hover any more (#867): the box sat where the number is, so it
 * caught clicks meant for the row — a play in single-click mode (#859),
 * the first click of a double-click otherwise — and it showed a checkbox
 * to every user on every pass of the mouse. Keyboard focus still reveals
 * it, so a selection can be started from the keyboard.
 *
 * Its own click and Space/Enter never reach the row, which would start
 * playback or reset the selection.
 */
export function RowSelectCheckbox({
  checked,
  selectionActive,
  onToggle,
  title,
  children,
}: RowSelectCheckboxProps) {
  const { t } = useTranslation();
  const shown = selectionActive;

  const toggle = (event: MouseEvent | KeyboardEvent) => {
    event.stopPropagation();
    event.preventDefault();
    onToggle();
  };

  return (
    <span className="relative flex items-center justify-end">
      <span
        className={`flex items-center justify-end ${shown ? "invisible" : ""}`}
      >
        {children}
      </span>
      <span
        role="checkbox"
        tabIndex={0}
        aria-checked={checked}
        aria-label={t("selection.selectRow", { title })}
        title={t("selection.selectRow", { title })}
        onClick={toggle}
        onDoubleClick={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === " " || event.key === "Enter") toggle(event);
        }}
        className={`absolute right-0 flex h-4 w-4 items-center justify-center rounded border transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${
          // Opacity, not visibility: a hidden element takes no focus,
          // and the keyboard must be able to start a selection too.
          // Not clickable while unseen, so a click on the number reaches
          // the row; the keyboard still reaches the box.
          shown
            ? ""
            : "pointer-events-none opacity-0 focus-visible:pointer-events-auto focus-visible:opacity-100"
        } ${
          checked
            ? "border-blue-500 bg-blue-500 text-white"
            : "border-zinc-400 bg-transparent text-transparent hover:border-zinc-600 dark:border-zinc-500 dark:hover:border-zinc-300"
        }`}
      >
        <Check size={12} strokeWidth={3} aria-hidden="true" />
      </span>
    </span>
  );
}

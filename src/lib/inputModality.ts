/**
 * Whether the user's last gesture was a pointer or a key, for the few
 * places that must tell the two apart.
 *
 * The case it exists for (#859): a row clicked with the mouse holds focus
 * without a ring, until a key is pressed — then the browser switches to
 * keyboard mode and `:focus-visible` paints the ring on it. Escape, which
 * clears a selection, is such a key, so a mouse user saw a ring appear
 * from nowhere. Escape and bare modifiers do not count as keyboard use
 * here: they navigate nothing.
 */
export type InputModality = "pointer" | "keyboard";

let last: InputModality = "pointer";

const IGNORED_KEYS = new Set(["Escape", "Shift", "Control", "Alt", "Meta"]);

if (typeof window !== "undefined") {
  window.addEventListener("pointerdown", () => (last = "pointer"), true);
  window.addEventListener(
    "keydown",
    (event) => {
      if (!IGNORED_KEYS.has(event.key)) last = "keyboard";
    },
    true,
  );
}

export function lastInputModality(): InputModality {
  return last;
}

/**
 * Drop the focus a mouse click left on a selectable track row (or on its
 * checkbox), so a key press does not paint a ring on it. Focus reached
 * from the keyboard is kept: that ring is how a keyboard user knows where
 * they are. Anything else keeps its focus too — a button clicked outside
 * the list is where the next Tab starts from.
 */
export function blurPointerFocusOnRow(): void {
  if (last !== "pointer") return;
  const active = document.activeElement;
  if (!(active instanceof HTMLElement)) return;
  const onRow =
    active.matches("[data-row-select]") ||
    active.querySelector(":scope [data-row-select]") !== null;
  if (onRow) active.blur();
}

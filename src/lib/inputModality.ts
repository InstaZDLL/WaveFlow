/**
 * Focus rings for keyboard focus only.
 *
 * A row or a button clicked with the mouse takes focus without a ring.
 * But the browser paints `:focus-visible` on it the moment any key is
 * pressed — it reads the key as keyboard navigation — and the ring then
 * stays until the next click. Escape did it (#859), and so did every
 * shortcut of the desktop around the app (#866): under Hyprland most of
 * them go through the Super key, so Super+Q to open a terminal left a
 * green frame on the last track clicked, and digits, letters and F-keys
 * did the same.
 *
 * So the modality is decided when focus **arrives**: an element focused
 * right after a pointer press is marked `data-pointer-focus`, and
 * `app.css` takes the ring off a marked element whatever is pressed
 * afterwards. Focus that arrives from the keyboard (Tab, the arrows of a
 * list) carries no mark and keeps its ring, which is how a keyboard user
 * knows where they are.
 *
 * Imported once, for its listeners, from `main.tsx`.
 */
type InputModality = "pointer" | "keyboard";

let last: InputModality = "pointer";

/** Keys that navigate nothing: modifiers, and Escape. */
const IGNORED_KEYS = new Set([
  "Escape",
  "Shift",
  "Control",
  "Alt",
  "AltGraph",
  "Meta",
  "OS",
  "Super",
  "Hyper",
  "CapsLock",
]);

const MARK = "pointerFocus";

if (typeof window !== "undefined") {
  window.addEventListener("pointerdown", () => (last = "pointer"), true);
  window.addEventListener(
    "keydown",
    (event) => {
      if (!IGNORED_KEYS.has(event.key)) last = "keyboard";
    },
    true,
  );
  document.addEventListener(
    "focusin",
    (event) => {
      const target = event.target;
      if (!(target instanceof HTMLElement)) return;
      if (last === "pointer") {
        target.dataset[MARK] = "";
      } else if (event.relatedTarget !== null) {
        // Focus moved here from another element by a key: a keyboard
        // focus, ring and all. With no `relatedTarget` it came back from
        // outside the page — the window was minimised or left — onto
        // the element that had it, which keeps whatever mark it had: the
        // last key pressed before leaving says nothing about how it was
        // focused.
        delete target.dataset[MARK];
      }
    },
    true,
  );
  document.addEventListener(
    "focusout",
    (event) => {
      const target = event.target;
      if (!(target instanceof HTMLElement)) return;
      // Only once focus has really gone elsewhere in the page. When the
      // whole window loses focus, the element stays the active one and
      // gets it back on return; dropping the mark here is what put the
      // ring on a clicked row after minimising and restoring (#866).
      setTimeout(() => {
        if (document.activeElement !== target) delete target.dataset[MARK];
      }, 0);
    },
    true,
  );
}

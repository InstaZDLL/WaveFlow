import { getProfileSetting, setProfileSetting } from "./tauri/profile";

/**
 * All actions that can be bound to a keyboard shortcut. The hook
 * `useGlobalShortcuts` knows how to dispatch each one against
 * `PlayerContext`. Adding a new action means: (1) extend this union,
 * (2) add a default binding, (3) add a `case` in the dispatcher,
 * (4) add an i18n label, (5) optionally add a row to the Settings
 * view.
 */
export type ShortcutAction =
  | "togglePlayback"
  | "next"
  | "previous"
  | "volumeUp"
  | "volumeDown"
  | "toggleMute"
  | "toggleShuffle"
  | "cycleRepeat"
  | "toggleQueue"
  | "toggleNowPlaying"
  | "toggleLyrics"
  | "toggleLike";

export const SHORTCUT_ACTIONS: ShortcutAction[] = [
  "togglePlayback",
  "next",
  "previous",
  "volumeUp",
  "volumeDown",
  "toggleMute",
  "toggleShuffle",
  "cycleRepeat",
  "toggleQueue",
  "toggleNowPlaying",
  "toggleLyrics",
  "toggleLike",
];

/**
 * Combo string format: optional `Ctrl+` / `Shift+` / `Alt+` modifiers
 * (in that fixed order), followed by a single key token. Key tokens are
 * either `event.key` for special keys (`Space`, `ArrowLeft`,
 * `ArrowRight`, `ArrowUp`, `ArrowDown`, `Enter`, `Escape`) or the
 * uppercase form of a letter / digit. Empty string = unbound.
 */
export const DEFAULT_BINDINGS: Record<ShortcutAction, string> = {
  togglePlayback: "Space",
  next: "ArrowRight",
  previous: "ArrowLeft",
  volumeUp: "ArrowUp",
  volumeDown: "ArrowDown",
  toggleMute: "M",
  toggleShuffle: "S",
  cycleRepeat: "R",
  toggleQueue: "Q",
  toggleNowPlaying: "N",
  toggleLyrics: "L",
  toggleLike: "Shift+L",
};

const SETTING_KEY = "ui.shortcuts";
export const SHORTCUTS_CHANGED_EVENT = "waveflow:shortcuts-changed";

export type ShortcutBindings = Record<ShortcutAction, string>;

/** Build a canonical combo string from a KeyboardEvent. */
export function comboFromEvent(event: KeyboardEvent): string {
  const parts: string[] = [];
  if (event.ctrlKey || event.metaKey) parts.push("Ctrl");
  if (event.shiftKey) parts.push("Shift");
  if (event.altKey) parts.push("Alt");

  let key = event.key;
  // Reject pure modifier presses — those aren't a complete combo.
  if (key === "Control" || key === "Meta" || key === "Shift" || key === "Alt") {
    return "";
  }
  if (key === " ") key = "Space";
  else if (key.length === 1) key = key.toUpperCase();
  // Else: keep `ArrowLeft`, `Enter`, `Escape`, function keys, etc. as-is.

  parts.push(key);
  return parts.join("+");
}

/** Render a combo for display. Just splits on `+` so the UI can put
 *  each token in its own `<kbd>`. */
export function comboParts(combo: string): string[] {
  if (!combo) return [];
  return combo.split("+");
}

export async function loadBindings(): Promise<ShortcutBindings> {
  try {
    const raw = await getProfileSetting(SETTING_KEY);
    if (!raw) return { ...DEFAULT_BINDINGS };
    const parsed = JSON.parse(raw) as Partial<ShortcutBindings>;
    return { ...DEFAULT_BINDINGS, ...parsed };
  } catch (err) {
    console.error("[shortcuts] load failed", err);
    return { ...DEFAULT_BINDINGS };
  }
}

export async function saveBindings(bindings: ShortcutBindings): Promise<void> {
  // Persist only the user's overrides — keeps the row tidy and means
  // future default changes (rare) get picked up automatically for any
  // action the user hasn't customised.
  const overrides: Partial<ShortcutBindings> = {};
  for (const action of SHORTCUT_ACTIONS) {
    if (bindings[action] !== DEFAULT_BINDINGS[action]) {
      overrides[action] = bindings[action];
    }
  }
  await setProfileSetting(SETTING_KEY, JSON.stringify(overrides), "string");
  // Notify any active hook to re-read without polling. Same pattern as
  // the sleep-timer / A-B loop visibility events.
  window.dispatchEvent(new CustomEvent(SHORTCUTS_CHANGED_EVENT));
}

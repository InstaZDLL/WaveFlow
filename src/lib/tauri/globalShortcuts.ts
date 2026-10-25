import { invoke } from "@tauri-apps/api/core";

import { announceSettingWrite } from "../settingWrites";

/** What a system-wide shortcut can do. The ids are the in-app shortcuts'
 *  own, so the rows reuse their labels. */
export type GlobalShortcutAction =
  | "togglePlayback"
  | "next"
  | "previous"
  | "toggleLike"
  | "volumeUp"
  | "volumeDown";

export const GLOBAL_SHORTCUT_ACTIONS: GlobalShortcutAction[] = [
  "togglePlayback",
  "next",
  "previous",
  "toggleLike",
  "volumeUp",
  "volumeDown",
];

/** `native`: the OS registers the combination. `portal`: a Wayland
 *  desktop, which confirms each change itself. `unavailable`: neither. */
export type GlobalShortcutBackend = "native" | "portal" | "unavailable";

export type ComboError = "empty" | "unsupportedKey" | "needsModifier";

export type GlobalShortcutState =
  | { kind: "active" }
  | { kind: "taken" }
  | { kind: "refused"; message: string }
  | { kind: "invalid"; reason: ComboError }
  | { kind: "dismissed" }
  | { kind: "suspended" }
  | { kind: "unavailable" };

export interface GlobalShortcutBinding {
  action: GlobalShortcutAction;
  combo: string;
  state: GlobalShortcutState;
  /** What the desktop assigned through the portal, in its own words. */
  trigger: string | null;
}

export interface GlobalShortcutsSnapshot {
  backend: GlobalShortcutBackend;
  bindings: GlobalShortcutBinding[];
  /** A combination asked for and not kept, with why. */
  rejected: {
    action: GlobalShortcutAction;
    combo: string;
    state: GlobalShortcutState;
  } | null;
}

export function globalShortcutsStatus(): Promise<GlobalShortcutsSnapshot> {
  return invoke<GlobalShortcutsSnapshot>("global_shortcuts_status");
}

/** Bind `action` to `combo` (`Ctrl+Alt+P`), or unbind it with `null`. */
export function setGlobalShortcut(
  action: GlobalShortcutAction,
  combo: string | null,
): Promise<GlobalShortcutsSnapshot> {
  return announceSettingWrite(
    invoke<GlobalShortcutsSnapshot>("global_shortcuts_set", { action, combo }),
  );
}

/** Set the registrations aside while a combination is recorded, so one
 *  WaveFlow already holds reaches the page instead of running. */
export function suspendGlobalShortcuts(
  suspended: boolean,
): Promise<GlobalShortcutsSnapshot> {
  return invoke<GlobalShortcutsSnapshot>("global_shortcuts_suspend", {
    suspended,
  });
}

export function clearGlobalShortcuts(): Promise<GlobalShortcutsSnapshot> {
  return announceSettingWrite(
    invoke<GlobalShortcutsSnapshot>("global_shortcuts_clear"),
  );
}

/** Keys a letter is taken from `KeyboardEvent.key` for: the backends
 *  register the key that types it, whatever the layout. */
const LETTER = /^[a-z]$/i;

/** `KeyboardEvent.code` → stored token, for the keys that are not
 *  letters. Mirrors `KEYS` in `global_shortcuts/combo.rs`. */
function tokenFromCode(code: string): string | null {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  if (/^Numpad[0-9]$/.test(code)) return code;
  switch (code) {
    case "Space":
    case "ArrowUp":
    case "ArrowDown":
    case "ArrowLeft":
    case "ArrowRight":
    case "Home":
    case "End":
    case "PageUp":
    case "PageDown":
    case "Insert":
    case "Delete":
    case "NumpadAdd":
    case "NumpadSubtract":
    case "NumpadMultiply":
    case "NumpadDivide":
      return code;
    default:
      return null;
  }
}

/**
 * The stored spelling of a key press: `Ctrl+Alt+Shift+Super+<key>`.
 * Returns `""` while only modifiers are held, `null` for a key a global
 * shortcut cannot use (the backend refuses the rest, such as a missing
 * modifier, with its reason).
 */
export function globalComboFromEvent(event: KeyboardEvent): string | null {
  if (["Control", "Shift", "Alt", "Meta", "AltGraph"].includes(event.key)) {
    return "";
  }
  const key = LETTER.test(event.key)
    ? event.key.toUpperCase()
    : tokenFromCode(event.code);
  if (!key) return null;
  const parts: string[] = [];
  if (event.ctrlKey) parts.push("Ctrl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  if (event.metaKey) parts.push("Super");
  parts.push(key);
  return parts.join("+");
}

const IS_MAC =
  typeof navigator !== "undefined" && /macintosh/i.test(navigator.userAgent);
const IS_WINDOWS =
  typeof navigator !== "undefined" && /windows/i.test(navigator.userAgent);

/** The tokens of a combination as the keyboard labels them here. */
export function globalComboParts(combo: string): string[] {
  return combo.split("+").map((part) => {
    if (part === "Super") return IS_MAC ? "Cmd" : IS_WINDOWS ? "Win" : "Super";
    if (part === "Alt" && IS_MAC) return "Option";
    if (part.startsWith("Arrow")) {
      return { ArrowUp: "↑", ArrowDown: "↓", ArrowLeft: "←", ArrowRight: "→" }[
        part
      ] as string;
    }
    if (part.startsWith("Numpad") && part.length > 6) {
      const rest = part.slice(6);
      const symbol =
        { Add: "+", Subtract: "−", Multiply: "×", Divide: "÷" }[rest] ?? rest;
      return `Num ${symbol}`;
    }
    return part;
  });
}

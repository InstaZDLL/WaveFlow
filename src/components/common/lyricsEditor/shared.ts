import type { LyricsDocument } from "../../../lib/tauri/lyricsDocument";

/** Apply an edit to the document. Edits sharing a `key` close together
 *  (typing in one field) undo as one step. */
export type EditDocument = (
  update: (doc: LyricsDocument) => LyricsDocument,
  key?: string,
) => void;

/** What the Sync step stamps with Space: a line's start, or a unit's. */
export type SyncMode = "line" | "word";

/** The next unit to stamp: a line, and a unit inside it in word mode. */
export interface SyncCursor {
  line: number;
  word: number;
}

/** `m:ss.cc`, or a dash for no time. */
export function formatEditorTime(ms: number | undefined): string {
  if (ms === undefined || !Number.isFinite(ms)) return "–:––.––";
  const cs = Math.round(Math.max(0, ms) / 10);
  const minutes = Math.floor(cs / 6000);
  const seconds = Math.floor(cs / 100) % 60;
  return `${minutes}:${String(seconds).padStart(2, "0")}.${String(cs % 100).padStart(2, "0")}`;
}

/** Colours told apart in a duet, one per singer in declaration order. */
const AGENT_COLORS = ["#f59e0b", "#38bdf8", "#a78bfa", "#34d399", "#f472b6"];
export const agentColor = (index: number) =>
  AGENT_COLORS[index % AGENT_COLORS.length];

/**
 * `onMouseDown` for the controls used while syncing (play, the mode, the
 * steps): a mouse click leaves the focus where it was, so Space and Enter
 * keep stamping instead of pressing the button last clicked. Tab still
 * reaches them, and a button focused that way keeps its keys.
 */
export function keepFocus(e: { preventDefault: () => void }): void {
  e.preventDefault();
}

/** Whether a key event belongs to a field being typed in. */
export function isTypingTarget(target: EventTarget | null): boolean {
  return (
    target instanceof HTMLElement &&
    (target.tagName === "INPUT" ||
      target.tagName === "TEXTAREA" ||
      target.tagName === "SELECT" ||
      target.isContentEditable)
  );
}

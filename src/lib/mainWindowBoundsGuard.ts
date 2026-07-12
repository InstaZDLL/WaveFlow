/**
 * Tiny coordination primitive between the debounced main-window bounds
 * writer ([`useMainWindowBounds`](../hooks/useMainWindowBounds.ts)) and
 * the manual "Reset window position" action
 * ([`WindowBoundsCard`](../components/views/settings/WindowBoundsCard.tsx)).
 *
 * The writer debounces saves at 300 ms and serializes them through a
 * promise chain — both of which live inside the hook's effect closure and
 * aren't reachable from the settings card. Without coordination, a save
 * scheduled just before a reset could land *after* `clearMainWindowBounds`
 * deletes the row and re-create it, so the next launch would restore the
 * bounds the user just reset (#362).
 *
 * The reset opens a short suppression window before deleting; the writer
 * checks it right before persisting and drops the write if active. This
 * covers the common case (a pending debounce timer that fires after the
 * delete) with a module-level flag instead of threading state across two
 * unrelated component trees.
 */
let suppressUntil = 0;

/**
 * Suppress main-window bounds persistence for `ms` milliseconds. Called by
 * the reset action just before `clearMainWindowBounds` so any in-flight or
 * debounced save is dropped instead of resurrecting the deleted row.
 */
export function suppressMainWindowBoundsWrites(ms = 1000): void {
  suppressUntil = Date.now() + ms;
}

/** `true` while a reset-initiated suppression window is still open. */
export function mainWindowBoundsWritesSuppressed(): boolean {
  return Date.now() < suppressUntil;
}

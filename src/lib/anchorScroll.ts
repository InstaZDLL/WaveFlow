/**
 * Whether a scroll heard by a capture-phase `scroll` listener moved what a
 * floating menu is anchored to.
 *
 * A `scroll` event does not bubble, so a menu that closes when its anchor
 * moves listens on `window` in the capture phase, and then hears every
 * scroll in the page: the synced lyrics panel scrolling to the next line
 * every few seconds closed a menu opened over the library (#1135). Only a
 * scroll of the document, or of an element that holds the anchor, moves it.
 */

/** The anchor is a point in the viewport, as a right-click leaves it. */
export function scrollMovesPoint(event: Event, x: number, y: number): boolean {
  const target = event.target;
  if (!(target instanceof Element)) return true;
  const r = target.getBoundingClientRect();
  return x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
}

/** The anchor is the element the menu opened from. */
export function scrollMovesElement(
  event: Event,
  anchor: Element | null,
): boolean {
  const target = event.target;
  if (!(target instanceof Element)) return true;
  return anchor === null || target.contains(anchor);
}

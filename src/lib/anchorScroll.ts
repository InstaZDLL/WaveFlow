/**
 * Whether a scroll heard by a capture-phase `scroll` listener moved what a
 * floating menu is anchored to.
 *
 * A `scroll` event does not bubble, so a menu that closes when its anchor
 * moves listens on `window` in the capture phase, and then hears every
 * scroll in the page: the synced lyrics panel scrolling to the next line
 * every few seconds closed a menu opened over the library (#1135). Only a
 * scroll of the document, or of an element that holds the anchor, moves it.
 * With no anchor every scroll counts, as before, and so with one the page
 * has since removed (its row filtered out or reloaded): no element holds
 * it any more, and the menu would otherwise stay open through any scroll.
 */
export function scrollMovesElement(
  event: Event,
  anchor: Element | null,
): boolean {
  const target = event.target;
  if (!(target instanceof Element)) return true;
  return anchor === null || !anchor.isConnected || target.contains(anchor);
}

/**
 * The element a menu opened at a viewport point stands for: the topmost
 * one there that is not part of a menu (`skip`, a selector), which the
 * menu itself covers once it is placed. A scrolling panel that only
 * overlaps the point, under it, is not it.
 */
export function elementUnder(
  x: number,
  y: number,
  skip: string,
): Element | null {
  return (
    document.elementsFromPoint(x, y).find((el) => !el.closest(skip)) ?? null
  );
}

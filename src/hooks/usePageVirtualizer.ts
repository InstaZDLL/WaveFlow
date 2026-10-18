import { useLayoutEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { usePageScroll } from "./usePageScroll";

interface PageVirtualizerOptions {
  count: number;
  estimateSize: (index: number) => number;
  overscan?: number;
  /** Ties a measured size to an item rather than to a position, for a
   *  list whose rows are measured (`measureElement`) and can shift. */
  getItemKey?: (index: number) => string | number;
}

/**
 * A row virtualizer driven by the page scroller (`usePageScroll()`), for
 * a list that sits somewhere down a view rather than owning a scroller of
 * its own — the one-scrollbar rule (see `docs/architecture/invariants.md`
 * § Virtual scroll everywhere).
 *
 * Attach `parentRef` to the element the rows are positioned in, give it
 * `position: relative` and `virtualizer.getTotalSize()` as its height, and
 * place each row at `virtualRow.start - scrollMargin`. `scrollMargin` is
 * how far down the scroller that element starts; it is re-measured when
 * the element, the scroller, or the view's content changes size — the
 * last one because a header or a section above the list growing (a bio
 * expanding, a grid loading) moves the list without resizing it.
 */
export function usePageVirtualizer<E extends HTMLElement = HTMLDivElement>({
  count,
  estimateSize,
  overscan = 8,
  getItemKey,
}: PageVirtualizerOptions) {
  "use no memo";
  const pageScrollRef = usePageScroll();
  const parentRef = useRef<E>(null);
  const [scrollMargin, setScrollMargin] = useState(0);
  useLayoutEffect(() => {
    const parent = parentRef.current;
    const scroller = pageScrollRef?.current;
    if (!parent || !scroller) return;
    const recompute = () => {
      const parentRect = parent.getBoundingClientRect();
      const scrollerRect = scroller.getBoundingClientRect();
      setScrollMargin(parentRect.top - scrollerRect.top + scroller.scrollTop);
    };
    recompute();
    const ro = new ResizeObserver(recompute);
    ro.observe(parent);
    ro.observe(scroller);
    for (const child of Array.from(scroller.children)) ro.observe(child);
    return () => ro.disconnect();
    // `count` re-runs this when the list first mounts: a view that shows
    // an empty state until its rows load has no parent to measure before.
  }, [pageScrollRef, count]);

  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count,
    getScrollElement: () => pageScrollRef?.current ?? null,
    estimateSize,
    overscan,
    scrollMargin,
    ...(getItemKey ? { getItemKey } : {}),
  });

  return { parentRef, virtualizer, scrollMargin };
}

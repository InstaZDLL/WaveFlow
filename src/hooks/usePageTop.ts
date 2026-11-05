import { useLayoutEffect } from "react";

import { usePageScroll } from "./usePageScroll";

/**
 * Opens a page at its top. A view that swaps one page for another inside itself keeps
 * the page's scroller, and with it how far down the last page was left.
 */
export function usePageTop(): void {
  const pageScrollRef = usePageScroll();
  useLayoutEffect(() => {
    pageScrollRef?.current?.scrollTo({ top: 0 });
  }, [pageScrollRef]);
}

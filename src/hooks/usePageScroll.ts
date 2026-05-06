import { useContext } from "react";
import { PageScrollContext } from "../contexts/PageScrollContext";

/** Returns AppLayout's main scrollable element ref, or null if no provider. */
export function usePageScroll() {
  return useContext(PageScrollContext);
}

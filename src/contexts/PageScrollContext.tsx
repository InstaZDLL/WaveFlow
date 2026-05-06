import { createContext, type RefObject } from "react";

// Ref to AppLayout's main scrollable area. Virtualized lists deep in the
// view tree consume this so they can drive a single page-level scrollbar
// instead of nesting their own — that's what creates the Spotify-style
// "header scrolls with content" feel and avoids double scrollbars.
export const PageScrollContext = createContext<RefObject<HTMLDivElement | null> | null>(
  null,
);

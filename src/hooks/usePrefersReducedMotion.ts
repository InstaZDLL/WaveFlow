import { useSyncExternalStore } from "react";

/**
 * Reactive `prefers-reduced-motion` read. Surfaces that autoplay motion
 * (the Canvas loop, issue #442) gate on this so a user who asked the OS to
 * reduce motion never gets an unsolicited looping video — they see the
 * static cover instead.
 */
const QUERY = "(prefers-reduced-motion: reduce)";

function subscribe(cb: () => void): () => void {
  if (typeof window === "undefined" || !window.matchMedia) return () => {};
  const mql = window.matchMedia(QUERY);
  mql.addEventListener("change", cb);
  return () => mql.removeEventListener("change", cb);
}

function getSnapshot(): boolean {
  if (typeof window === "undefined" || !window.matchMedia) return false;
  return window.matchMedia(QUERY).matches;
}

export function usePrefersReducedMotion(): boolean {
  return useSyncExternalStore(subscribe, getSnapshot, () => false);
}

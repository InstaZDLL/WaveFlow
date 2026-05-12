import { useEffect, useRef } from "react";

/**
 * Modal accessibility helper. Wires up the three behaviours every
 * modal in the app needs:
 *
 * 1. **Escape closes the modal.** Skipped automatically when an
 *    inner control swallows the event (e.g. an open `<select>`)
 *    because the listener fires on the document, not on `window`,
 *    and bubbles cleanly.
 * 2. **Focus is trapped inside the modal.** Tab / Shift+Tab cycle
 *    through the focusable descendants of the returned ref instead
 *    of leaking back to the page underneath.
 * 3. **Focus is restored on close.** The element that had focus when
 *    the modal opened gets it back so keyboard-only users don't end
 *    up at the top of the document.
 *
 * Returns a ref the caller attaches to the modal's container — the
 * element whose subtree should hold the focus.
 */
export function useModalA11y<T extends HTMLElement>(
  isOpen: boolean,
  onClose: () => void,
) {
  const containerRef = useRef<T | null>(null);
  const lastFocusedRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!isOpen) return;

    // Stash whoever had focus, then move focus into the modal so
    // screen readers announce its content immediately.
    lastFocusedRef.current = document.activeElement as HTMLElement | null;
    const container = containerRef.current;
    if (container) {
      const first = getFocusable(container)[0] ?? container;
      // setTimeout 0 because the modal may animate in; without it
      // some browsers race and focus an element that's about to be
      // re-rendered.
      window.setTimeout(() => first.focus(), 0);
    }

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
        return;
      }
      if (e.key !== "Tab" || !containerRef.current) return;
      const focusables = getFocusable(containerRef.current);
      if (focusables.length === 0) {
        e.preventDefault();
        return;
      }
      const first = focusables[0];
      const last = focusables[focusables.length - 1];
      const active = document.activeElement as HTMLElement | null;
      if (e.shiftKey) {
        if (active === first || !containerRef.current.contains(active)) {
          e.preventDefault();
          last.focus();
        }
      } else if (active === last) {
        e.preventDefault();
        first.focus();
      }
    };

    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      // Restore focus on close. Guarded against the previous element
      // being detached (e.g. a context menu that spawned the modal
      // and then re-rendered).
      const prev = lastFocusedRef.current;
      if (prev && document.contains(prev)) prev.focus();
    };
  }, [isOpen, onClose]);

  return containerRef;
}

/** Tab-order list of focusable descendants. Mirrors the WAI-ARIA
 * authoring-practices selector with the practical exclusions
 * (negative tabindex, disabled, hidden). */
function getFocusable(root: HTMLElement): HTMLElement[] {
  const SELECTOR = [
    "a[href]",
    "button:not([disabled])",
    "input:not([disabled]):not([type='hidden'])",
    "select:not([disabled])",
    "textarea:not([disabled])",
    "[tabindex]:not([tabindex='-1'])",
  ].join(",");
  const nodes = Array.from(root.querySelectorAll<HTMLElement>(SELECTOR)).filter(
    (el) => {
      if (el.hasAttribute("aria-hidden")) return false;
      // `offsetParent` is null for `display: none` ancestors. Cheaper
      // than `getComputedStyle` in a tab-trap loop.
      return el.offsetParent !== null || el === document.activeElement;
    },
  );
  return nodes;
}

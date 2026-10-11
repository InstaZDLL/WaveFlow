import { useEffect, useRef } from "react";

/**
 * Ctrl+A (Cmd+A on macOS) selects every row of the list on screen.
 *
 * Left to the field when one has focus — selecting its text is what the
 * combo means there — and to an open dialog, whose Ctrl+A must not
 * select the list behind it. `enabled` is the caller's own gate: the
 * tab or view whose list this is must be the one showing.
 */
export function useSelectAllShortcut(
  enabled: boolean,
  onSelectAll: () => void,
): void {
  const handlerRef = useRef(onSelectAll);
  useEffect(() => {
    handlerRef.current = onSelectAll;
  }, [onSelectAll]);

  useEffect(() => {
    if (!enabled) return;
    const onKey = (event: KeyboardEvent) => {
      if (!(event.ctrlKey || event.metaKey) || event.shiftKey || event.altKey) {
        return;
      }
      if (event.key.toLowerCase() !== "a") return;
      const target = event.target as HTMLElement | null;
      if (
        target &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.tagName === "SELECT" ||
          target.isContentEditable)
      ) {
        return;
      }
      if (document.querySelector('[role="dialog"][aria-modal="true"]')) return;
      event.preventDefault();
      handlerRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [enabled]);
}

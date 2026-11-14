import {
  type RefObject,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";

/** Keys that scroll the element holding the focus, or its scroller. */
const SCROLL_KEYS = new Set([
  "ArrowUp",
  "ArrowDown",
  "PageUp",
  "PageDown",
  "Home",
  "End",
]);

interface LyricsFollowOptions {
  /** The scroller holding the lines. */
  containerRef: RefObject<HTMLElement | null>;
  /** One element per line, indexed like the lines. */
  lineRefs: RefObject<Array<HTMLElement | null>>;
  activeIndex: number;
  /** Whether there is a sung line to follow at all: synced lyrics, shown. */
  enabled: boolean;
  /** What the lyrics belong to — the track, the document. Any of them
   *  changing ends the browsing. */
  resetKeys: readonly unknown[];
}

export interface LyricsFollow {
  /** The user has scrolled away and the view no longer follows the line. */
  browsing: boolean;
  /** Following again, from the next change of line or now (a seek). */
  resume: () => void;
  /** The "back to playback" button: following again, the focus handed to
   *  the sung line since the button it was on goes away. */
  backToLine: () => void;
}

/**
 * Keeps the sung line centred in a lyrics scroller (#1096), until the user
 * scrolls the lyrics themselves: then the view stays where they took it
 * while the line and the karaoke go on, and follows again when the sung
 * line comes into view of its own, when the user asks for it (`resume`),
 * or when the track changes.
 *
 * Only a gesture says the user is scrolling — wheel, touch, a press on the
 * scrollbar or the middle button, a scrolling key — never a `scroll` event:
 * the centring's own `scrollIntoView` fires those too.
 *
 * "Comes into view" means the sung line was out of view and is now in it.
 * Scrolled ahead, the music catches up with what the user is reading and the
 * view follows from there; scrolled back towards the start, the sung line
 * moves away and never comes back, and the view stays put until `resume`.
 */
export function useLyricsFollow({
  containerRef,
  lineRefs,
  activeIndex,
  enabled,
  resetKeys,
}: LyricsFollowOptions): LyricsFollow {
  // The key the browsing started under: it ends with the track it began on,
  // without an effect to clear it.
  const [browsingFor, setBrowsingFor] = useState<readonly unknown[] | null>(
    null,
  );
  // Hidden (the panel closed, the lyrics no longer synced) ends it too, so
  // the lyrics follow again when they come back.
  const [wasEnabled, setWasEnabled] = useState(enabled);
  if (wasEnabled !== enabled) {
    setWasEnabled(enabled);
    if (!enabled) setBrowsingFor(null);
  }
  const browsing =
    enabled &&
    browsingFor !== null &&
    browsingFor.length === resetKeys.length &&
    browsingFor.every((key, i) => Object.is(key, resetKeys[i]));
  // The listeners below read the keys at the moment of the gesture.
  const keysRef = useRef(resetKeys);
  useEffect(() => {
    keysRef.current = resetKeys;
  });

  // ── The user scrolling ────────────────────────────────────────────
  useEffect(() => {
    const container = containerRef.current;
    if (!enabled || !container) return;
    // Lyrics that fit have nowhere to be browsed to.
    const browse = () => {
      if (container.scrollHeight > container.clientHeight) {
        setBrowsingFor(keysRef.current);
      }
    };
    const onPointerDown = (e: PointerEvent) => {
      // The middle button starts the browser's own autoscroll.
      if (e.button === 1) {
        browse();
        return;
      }
      if (e.target !== container) return;
      // A press on the container outside its client box is on its
      // scrollbar, whichever side the reading direction puts it on.
      const rect = container.getBoundingClientRect();
      const left = rect.left + container.clientLeft;
      if (e.clientX < left || e.clientX >= left + container.clientWidth) {
        browse();
      }
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey) return;
      if (SCROLL_KEYS.has(e.key)) {
        browse();
        return;
      }
      // Space activates a focused line; it scrolls only the scroller itself.
      if (e.key === " " && e.target === container) browse();
    };
    container.addEventListener("wheel", browse, { passive: true });
    container.addEventListener("touchmove", browse, { passive: true });
    container.addEventListener("pointerdown", onPointerDown);
    container.addEventListener("keydown", onKeyDown);
    return () => {
      container.removeEventListener("wheel", browse);
      container.removeEventListener("touchmove", browse);
      container.removeEventListener("pointerdown", onPointerDown);
      container.removeEventListener("keydown", onKeyDown);
    };
  }, [containerRef, enabled]);

  // ── Following ─────────────────────────────────────────────────────
  useEffect(() => {
    if (!enabled || browsing || activeIndex < 0) return;
    lineRefs.current[activeIndex]?.scrollIntoView({
      behavior: "smooth",
      block: "center",
    });
  }, [activeIndex, enabled, browsing, lineRefs]);

  // ── The sung line coming into view ────────────────────────────────
  // Whether the sung line has been out of view since the browsing began.
  // Kept across line changes: the line before was out of view and this one
  // is in it is the music reaching what the user scrolled ahead to.
  const outOfView = useRef(false);
  useEffect(() => {
    if (!browsing) outOfView.current = false;
  }, [browsing]);
  useEffect(() => {
    const container = containerRef.current;
    const line = activeIndex >= 0 ? lineRefs.current[activeIndex] : null;
    if (!browsing || !container || !line) return;
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (!entry.isIntersecting) {
          outOfView.current = true;
        } else if (outOfView.current && entry.intersectionRatio >= 0.5) {
          setBrowsingFor(null);
        }
      },
      { root: container, threshold: [0, 0.5] },
    );
    observer.observe(line);
    return () => observer.disconnect();
  }, [browsing, activeIndex, containerRef, lineRefs]);

  const resume = useCallback(() => setBrowsingFor(null), []);
  const backToLine = useCallback(() => {
    setBrowsingFor(null);
    // Without scrolling: the centring effect glides there on its own.
    lineRefs.current[activeIndex]
      ?.querySelector<HTMLElement>("button")
      ?.focus({ preventScroll: true });
  }, [activeIndex, lineRefs]);

  return { browsing, resume, backToLine };
}

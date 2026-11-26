import {
  type RefObject,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";

import { useLyricsExpressiveMotion } from "./useLyricsExpressiveMotion";
import { usePrefersReducedMotion } from "./usePrefersReducedMotion";

/** How long each line takes to glide to its new place. */
const GLIDE_MS = 560;
/** Leaves fast, settles slowly on its place. */
const GLIDE_EASE = "cubic-bezier(0.41, 0, 0.12, 0.99)";
/** The lines below the sung one leave a beat later each, up to this many:
 *  the gaps open and close as the list moves on. */
const STAGGER_LINES = 3;
const STAGGER_MS = 40;
/** Lines this far outside the scroller are left where they are. */
const GLIDE_MARGIN_PX = 200;

/** How far a line is drawn from its place right now: its glide so far. */
function drawnOffset(line: HTMLElement): number {
  const transform = getComputedStyle(line).transform;
  return transform && transform !== "none"
    ? new DOMMatrixReadOnly(transform).m42
    : 0;
}

/** Back in place at once, a glide under way included. */
function settle(line: HTMLElement): void {
  line.style.transition = "none";
  line.style.transform = "";
}

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
 * the centring's own glide fires those too.
 *
 * By default the centring is the browser's smooth `scrollIntoView`, the
 * whole list in one block. With `lyrics.expressive_motion` on, it moves the
 * scroller to the sung line in one step instead, then has
 * each line in view glide from where it was drawn to its new place, the
 * lines below the sung one a beat later each: the list moves on line by
 * line instead of in one block. Only whole lines move, never a word (the
 * karaoke's words change colour only). A new line starts every glide from
 * wherever the last one had got to. Under `prefers-reduced-motion`, or
 * for a jump of more than a screen and a half (a seek), the lines are put
 * in place at once.
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
  // The key the browsing started under: it ends with the track it began on.
  const [browsingFor, setBrowsingFor] = useState<readonly unknown[] | null>(
    null,
  );
  const sameKeys =
    browsingFor !== null &&
    browsingFor.length === resetKeys.length &&
    browsingFor.every((key, i) => Object.is(key, resetKeys[i]));
  // Cleared as soon as the keys differ, not only masked: going from A to B
  // and back to A before B's lyrics load would otherwise bring back A's
  // browsing with no new gesture.
  if (browsingFor !== null && !sameKeys) setBrowsingFor(null);
  // Hidden (the panel closed, the lyrics no longer synced) ends it too, so
  // the lyrics follow again when they come back.
  const [wasEnabled, setWasEnabled] = useState(enabled);
  if (wasEnabled !== enabled) {
    setWasEnabled(enabled);
    if (!enabled) setBrowsingFor(null);
  }
  const browsing = enabled && sameKeys;
  const reducedMotion = usePrefersReducedMotion();
  const staggered = useLyricsExpressiveMotion();
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

  // ── The motion turned off ─────────────────────────────────────────
  // The setting turned off, or less motion asked for, while lines glide:
  // they go back in place at once, browsing or not (the centring below
  // waits for the next line, and does nothing while browsing).
  useLayoutEffect(() => {
    if (staggered && !reducedMotion) return;
    const first = lineRefs.current.find((row) => row != null);
    if (!first) return;
    Array.from(
      first.parentElement?.children ?? [first],
      (row) => row as HTMLElement,
    ).forEach(settle);
  }, [staggered, reducedMotion, lineRefs]);

  // ── Following ─────────────────────────────────────────────────────
  // Before the paint: the scroller's step and the lines' start offsets land
  // in the same frame, so the list is never seen in its new place first.
  useLayoutEffect(() => {
    if (!enabled || browsing || activeIndex < 0) return;
    const container = containerRef.current;
    const line = lineRefs.current[activeIndex];
    if (!container || !line) return;
    // Every row of the list moves with the step, the interlude dots between
    // two lines included, which have no line ref: the sung line's siblings.
    const lines = Array.from(
      line.parentElement?.children ?? [line],
      (row) => row as HTMLElement,
    );
    const sungAt = lines.indexOf(line);
    if (!staggered) {
      // Turned off mid-glide: the lines go back in place first.
      lines.forEach(settle);
      line.scrollIntoView({ behavior: "smooth", block: "center" });
      return;
    }
    // Less motion asked for mid-glide: the lines go back in place before
    // anything is measured, so a scroller already at its target still
    // stops them.
    if (reducedMotion) lines.forEach(settle);
    // Read everything before writing anything: one layout, not one a line.
    const box = container.getBoundingClientRect();
    const at = line.getBoundingClientRect();
    // The rectangles are drawn pixels, the scroller counts its own: on a
    // wide window the immersive view is enlarged with `zoom`, and the two
    // differ by that factor.
    const scale =
      container.offsetHeight > 0 ? box.height / container.offsetHeight : 1;
    const middle =
      box.top + (container.clientTop + container.clientHeight / 2) * scale;
    const before = container.scrollTop;
    // The line's own place, not where its glide is drawing it.
    const to = Math.min(
      container.scrollHeight - container.clientHeight,
      Math.max(
        0,
        before + (at.top + at.height / 2 - middle) / scale - drawnOffset(line),
      ),
    );
    if (Math.abs(to - before) < 1) return;
    // The lines in view before the step or after it: one that only comes
    // into view must glide in too, not appear in its place.
    const ahead = (to - before) * scale;
    const from = box.top - GLIDE_MARGIN_PX + Math.min(0, ahead);
    const until = box.bottom + GLIDE_MARGIN_PX + Math.max(0, ahead);
    const inView = lines.flatMap((row, i) => {
      const r = row.getBoundingClientRect();
      return r.bottom < from || r.top > until
        ? []
        : [{ row, i, offset: drawnOffset(row) }];
    });
    // `instant`: the immersive column's `scroll-smooth` would otherwise
    // animate the step itself.
    container.scrollTo({ top: to, behavior: "instant" });
    // What the scroller really moved, which its ends may have clamped.
    const moved = container.scrollTop - before;
    if (reducedMotion || Math.abs(moved) > container.clientHeight * 1.5) {
      lines.forEach(settle);
      return;
    }
    // Clamped at an end: nothing moved, the glides under way go on.
    if (Math.abs(moved) < 1) return;
    // Each line drawn where it was, then let go towards its place.
    for (const { row, offset } of inView) {
      row.style.transition = "none";
      row.style.transform = `translate3d(0, ${moved + offset}px, 0)`;
    }
    void container.offsetHeight;
    for (const { row, i } of inView) {
      const below = i - sungAt;
      const delay = Math.min(Math.max(below, 0), STAGGER_LINES) * STAGGER_MS;
      row.style.transition = `transform ${GLIDE_MS}ms ${GLIDE_EASE} ${delay}ms`;
      row.style.transform = "";
    }
  }, [
    activeIndex,
    enabled,
    browsing,
    containerRef,
    lineRefs,
    reducedMotion,
    staggered,
  ]);

  // ── The sung line coming into view ────────────────────────────────
  // Whether the sung line has been out of view (less than half of it shown)
  // since the browsing began.
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
      // A batch can hold several observations of the line, oldest first:
      // out of view then back in it within one callback still resumes.
      (entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting || entry.intersectionRatio < 0.5) {
            outOfView.current = true;
          } else if (outOfView.current) {
            setBrowsingFor(null);
            return;
          }
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

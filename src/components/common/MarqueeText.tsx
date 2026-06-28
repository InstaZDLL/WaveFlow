import { useEffect, useRef, useState, type CSSProperties } from "react";
import { useScrollLongTitles } from "../../hooks/useScrollLongTitles";

interface MarqueeTextProps {
  text: string;
  /** Applied to the (overflow-clipped) container — pass the font /
   *  colour / line-height classes here. */
  className?: string;
  /** Minimum overflow in px before scrolling kicks in, so a 1–2 px
   *  sub-pixel overflow doesn't trigger a pointless crawl. */
  threshold?: number;
}

/**
 * Single-line label that **scrolls when it overflows** instead of being
 * cut by an ellipsis — it glides end-to-end and back (ping-pong, with a
 * pause at each extremity) so long track titles stay fully readable. When
 * the text fits, it renders static and truncates as before. Respects
 * `prefers-reduced-motion` (the keyframe is disabled in app.css, so it
 * falls back to a static clipped line).
 *
 * Used by the PlayerBar + immersive now-playing titles. Measures overflow
 * via a `ResizeObserver` so it reacts to both track changes and the
 * container resizing (panel open, window resize).
 */
export function MarqueeText({ text, className, threshold = 4 }: MarqueeTextProps) {
  const { enabled } = useScrollLongTitles();
  const containerRef = useRef<HTMLSpanElement>(null);
  const textRef = useRef<HTMLSpanElement>(null);
  const [shift, setShift] = useState(0);

  useEffect(() => {
    // Skip measuring entirely when the user disabled scrolling — the
    // static truncate branch renders regardless of overflow.
    if (!enabled) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setShift(0);
      return;
    }
    const measure = () => {
      const container = containerRef.current;
      const inner = textRef.current;
      if (!container || !inner) return;
      const diff = inner.scrollWidth - container.clientWidth;
      setShift(diff > threshold ? diff : 0);
    };
    measure();
    const ro = new ResizeObserver(measure);
    if (containerRef.current) ro.observe(containerRef.current);
    if (textRef.current) ro.observe(textRef.current);
    return () => ro.disconnect();
  }, [text, threshold, enabled]);

  const animate = enabled && shift > 0;

  return (
    <span
      ref={containerRef}
      // `text-left` while scrolling so the run starts flush-left and the
      // translate reveals the tail; otherwise inherit the parent's
      // alignment (the immersive title is centred).
      className={`block overflow-hidden ${animate ? "text-left" : ""} ${className ?? ""}`}
    >
      <span
        ref={textRef}
        className={
          animate
            ? "inline-block whitespace-nowrap animate-marquee"
            : "block truncate"
        }
        style={
          animate
            ? ({
                "--marquee-shift": `${-shift}px`,
                // Roughly constant reading speed: scale the cycle with
                // the distance, clamped so short overflows aren't frantic
                // and very long ones aren't glacial.
                animationDuration: `${Math.min(28, Math.max(8, shift / 22 + 6))}s`,
              } as CSSProperties)
            : undefined
        }
      >
        {text}
      </span>
    </span>
  );
}

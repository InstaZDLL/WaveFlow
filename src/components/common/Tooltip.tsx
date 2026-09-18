import type { ReactNode } from "react";

interface TooltipProps {
  /** The text shown when the user hovers / focuses the child. */
  label: string;
  /** The element the tooltip is attached to — usually a button or icon. */
  children: ReactNode;
  /** Which side the bubble pops out on. Defaults to `bottom`. */
  side?: "top" | "bottom";
  /** Extra classes for the wrapping `div`. Tooltip anchors off this element. */
  className?: string;
}

/**
 * Lightweight CSS-only tooltip. Uses a scoped `group/tooltip` so nested
 * tooltips inside other group elements (e.g. the profile menu) don't
 * interfere with each other's hover state.
 *
 * Positioning is absolute off the wrapper and centered via
 * `left-1/2 -translate-x-1/2`. The bubble has `pointer-events-none` so it
 * never steals hover from the anchor element — critical when the user
 * drags across a toolbar of icon buttons.
 *
 * ## Why the bubble is scaled to nothing at rest (issue #677)
 *
 * `opacity-0` hides a box, it does not remove it. The bubble is
 * `whitespace-nowrap`, so it is as wide as its label, and it is centred
 * on an anchor that is usually a small icon button. On a control near the
 * right edge of a view the invisible bubble reached past that edge — and
 * an absolutely positioned descendant still counts toward the scrollable
 * overflow of its scrolling ancestor. So the page scroller grew to
 * contain something nobody could see, and painted a horizontal scrollbar
 * at window sizes where the layout plainly fitted. Measured in the
 * library: 63px of overflow, all of it from two bubbles, in both a
 * restored and a maximised window — the distance from the anchor to the
 * right edge does not change with the window, so neither did the bar.
 *
 * Scrollable overflow is computed from *transformed* border boxes, which
 * is why `-translate-x-1/2` already shifts what the scroller sees.
 * Scaling to zero collapses the box to a point, and with the origin on
 * the anchor's side that point lands on the anchor's own centre — so the
 * bubble contributes nothing at all until it is wanted.
 *
 * The scale is transitioned rather than snapped: snapping would make the
 * bubble vanish the instant the pointer leaves, where today it fades, and
 * the fade would have nothing left to fade. The transition names `scale`,
 * not `transform` — Tailwind v4 emits the standalone `scale` and
 * `translate` properties rather than composing them into `transform`, so
 * a `transition-[opacity,transform]` here would animate a property that
 * never changes and let the scale snap anyway. Under
 * `prefers-reduced-motion` the transition narrows to opacity alone and the
 * scale switches instantly, which is what that setting asks for.
 *
 * This is deliberately not the portal the overlay invariant calls for.
 * That rule is about `z-index` losing to a `backdrop-filter` ancestor;
 * nothing here is being covered up, and a portal would need JS
 * positioning per bubble, which is exactly what the CSS-only design
 * avoids.
 */
export function Tooltip({
  label,
  children,
  side = "bottom",
  className = "",
}: TooltipProps) {
  const position =
    side === "top"
      ? "bottom-full mb-2 origin-bottom"
      : "top-full mt-2 origin-top";
  return (
    <div className={`relative group/tooltip ${className}`}>
      {children}
      <div
        role="tooltip"
        className={`pointer-events-none absolute ${position} left-1/2 -translate-x-1/2 scale-0 px-2 py-1 rounded-md bg-zinc-900 text-white text-xs font-medium whitespace-nowrap opacity-0 group-hover/tooltip:scale-100 group-hover/tooltip:opacity-100 group-focus-within/tooltip:scale-100 group-focus-within/tooltip:opacity-100 transition-[opacity,scale] duration-150 motion-reduce:transition-opacity shadow-lg z-50 dark:bg-zinc-100 dark:text-zinc-900`}
      >
        {label}
      </div>
    </div>
  );
}

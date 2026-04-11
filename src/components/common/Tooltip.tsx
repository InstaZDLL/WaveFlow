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
 */
export function Tooltip({
  label,
  children,
  side = "bottom",
  className = "",
}: TooltipProps) {
  const position =
    side === "top"
      ? "bottom-full mb-2"
      : "top-full mt-2";
  return (
    <div className={`relative group/tooltip ${className}`}>
      {children}
      <div
        role="tooltip"
        className={`pointer-events-none absolute ${position} left-1/2 -translate-x-1/2 px-2 py-1 rounded-md bg-zinc-900 text-white text-xs font-medium whitespace-nowrap opacity-0 group-hover/tooltip:opacity-100 group-focus-within/tooltip:opacity-100 transition-opacity duration-150 shadow-lg z-50 dark:bg-zinc-100 dark:text-zinc-900`}
      >
        {label}
      </div>
    </div>
  );
}

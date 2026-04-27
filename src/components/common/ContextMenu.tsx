import {
  type ReactNode,
  type CSSProperties,
  type MouseEvent as ReactMouseEvent,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { ChevronRight } from "lucide-react";

/**
 * Anchor point for a context menu — typically the right-click event's
 * `clientX` / `clientY`. The menu positions itself so it stays within
 * the viewport, flipping its origin when needed.
 */
export interface ContextMenuPoint {
  x: number;
  y: number;
}

interface ContextMenuRootProps {
  point: ContextMenuPoint;
  onClose: () => void;
  children: ReactNode;
  /** Width hint for centering / flip math; defaults to 240px. */
  width?: number;
}

const MENU_VERTICAL_MARGIN = 8;
const MENU_HORIZONTAL_MARGIN = 8;

/**
 * Root container for a Spotify-style context menu. Renders at a viewport
 * point, traps Escape to close, and dismisses on click-outside.
 *
 * Positioning is computed after first render so we can read the actual
 * rendered size — the menu height varies with the number of items and
 * any submenu indicators.
 */
export function ContextMenu({
  point,
  onClose,
  children,
  width = 240,
}: ContextMenuRootProps) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<CSSProperties>({
    top: -9999,
    left: -9999,
    visibility: "hidden",
    position: "fixed",
    width,
  });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    let left = point.x;
    let top = point.y;
    if (left + rect.width + MENU_HORIZONTAL_MARGIN > vw) {
      left = Math.max(MENU_HORIZONTAL_MARGIN, vw - rect.width - MENU_HORIZONTAL_MARGIN);
    }
    if (top + rect.height + MENU_VERTICAL_MARGIN > vh) {
      top = Math.max(MENU_VERTICAL_MARGIN, vh - rect.height - MENU_VERTICAL_MARGIN);
    }
    setPos({ top, left, position: "fixed", width });
  }, [point.x, point.y, width]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    const onMouseDown = (e: MouseEvent) => {
      const target = e.target as HTMLElement | null;
      if (target?.closest("[data-context-menu]")) return;
      onClose();
    };
    // `mousedown` instead of `click` so dragging doesn't accidentally
    // dismiss before the user releases.
    window.addEventListener("keydown", onKey);
    window.addEventListener("mousedown", onMouseDown);
    // Close on scroll — keeps the menu glued to its anchor without
    // chasing the page.
    window.addEventListener("scroll", onClose, true);
    window.addEventListener("resize", onClose);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("mousedown", onMouseDown);
      window.removeEventListener("scroll", onClose, true);
      window.removeEventListener("resize", onClose);
    };
  }, [onClose]);

  return (
    <div
      ref={ref}
      data-context-menu
      role="menu"
      style={pos}
      className="z-100 rounded-lg border border-zinc-200 bg-white shadow-2xl dark:border-zinc-700 dark:bg-zinc-900 dark:shadow-black/60 py-1 text-sm animate-fade-in"
      onContextMenu={(e) => e.preventDefault()}
    >
      {children}
    </div>
  );
}

interface ContextMenuItemProps {
  icon?: ReactNode;
  label: string;
  shortcut?: string;
  disabled?: boolean;
  danger?: boolean;
  onSelect: () => void;
}

export function ContextMenuItem({
  icon,
  label,
  shortcut,
  disabled,
  danger,
  onSelect,
}: ContextMenuItemProps) {
  const handleClick = (e: ReactMouseEvent<HTMLButtonElement>) => {
    e.stopPropagation();
    if (disabled) return;
    onSelect();
  };
  return (
    <button
      type="button"
      role="menuitem"
      onClick={handleClick}
      disabled={disabled}
      className={`w-full flex items-center justify-between gap-3 px-3 py-2 text-left transition-colors ${
        disabled
          ? "text-zinc-300 dark:text-zinc-600 cursor-not-allowed"
          : danger
            ? "text-rose-500 hover:bg-rose-50 dark:hover:bg-rose-500/10"
            : "text-zinc-700 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800"
      }`}
    >
      <span className="flex items-center gap-3 min-w-0">
        {icon != null && (
          <span className="shrink-0 text-zinc-400 dark:text-zinc-500 flex items-center">
            {icon}
          </span>
        )}
        <span className="truncate">{label}</span>
      </span>
      {shortcut && (
        <span className="text-xs text-zinc-400 tabular-nums">{shortcut}</span>
      )}
    </button>
  );
}

export function ContextMenuSeparator() {
  return (
    <div
      role="separator"
      className="my-1 h-px bg-zinc-100 dark:bg-zinc-800"
    />
  );
}

interface ContextMenuSubProps {
  icon?: ReactNode;
  label: string;
  /** Renders the submenu content lazily when the user hovers/opens. */
  children: ReactNode;
  /** Width hint for the submenu panel. */
  width?: number;
}

/**
 * Item that opens a submenu when hovered (or focused). The submenu
 * pops to the right, flipping left if it would overflow the viewport.
 */
export function ContextMenuSub({
  icon,
  label,
  children,
  width = 220,
}: ContextMenuSubProps) {
  const [open, setOpen] = useState(false);
  const itemRef = useRef<HTMLButtonElement>(null);
  const subRef = useRef<HTMLDivElement>(null);
  const closeTimer = useRef<number | null>(null);
  const [subPos, setSubPos] = useState<CSSProperties>({
    visibility: "hidden",
    position: "fixed",
  });

  const cancelClose = useCallback(() => {
    if (closeTimer.current != null) {
      window.clearTimeout(closeTimer.current);
      closeTimer.current = null;
    }
  }, []);

  const scheduleClose = useCallback(() => {
    cancelClose();
    closeTimer.current = window.setTimeout(() => setOpen(false), 120);
  }, [cancelClose]);

  useLayoutEffect(() => {
    if (!open) return;
    const trigger = itemRef.current;
    const sub = subRef.current;
    if (!trigger || !sub) return;
    const tRect = trigger.getBoundingClientRect();
    const sRect = sub.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    let left = tRect.right;
    let top = tRect.top;
    if (left + sRect.width + MENU_HORIZONTAL_MARGIN > vw) {
      // Flip to the trigger's left edge.
      left = Math.max(MENU_HORIZONTAL_MARGIN, tRect.left - sRect.width);
    }
    if (top + sRect.height + MENU_VERTICAL_MARGIN > vh) {
      top = Math.max(MENU_VERTICAL_MARGIN, vh - sRect.height - MENU_VERTICAL_MARGIN);
    }
    setSubPos({ top, left, position: "fixed", width });
  }, [open, width]);

  return (
    <div
      onMouseEnter={() => {
        cancelClose();
        setOpen(true);
      }}
      onMouseLeave={scheduleClose}
    >
      <button
        ref={itemRef}
        type="button"
        role="menuitem"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="w-full flex items-center justify-between gap-3 px-3 py-2 text-left text-zinc-700 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
      >
        <span className="flex items-center gap-3 min-w-0">
          {icon != null && (
            <span className="shrink-0 text-zinc-400 dark:text-zinc-500 flex items-center">
              {icon}
            </span>
          )}
          <span className="truncate">{label}</span>
        </span>
        <ChevronRight size={14} className="text-zinc-400" />
      </button>
      {open && (
        <div
          ref={subRef}
          data-context-menu
          role="menu"
          style={subPos}
          className="z-101 rounded-lg border border-zinc-200 bg-white shadow-2xl dark:border-zinc-700 dark:bg-zinc-900 dark:shadow-black/60 py-1 animate-fade-in"
          onMouseEnter={cancelClose}
          onMouseLeave={scheduleClose}
        >
          {children}
        </div>
      )}
    </div>
  );
}


import {
  ChevronLeft,
  ChevronRight,
  Moon,
  PanelLeftClose,
  PanelLeftOpen,
  Search,
} from "lucide-react";
import { useFe } from "../useFe";

/**
 * The top bar loses its glass and its border: it is the first 52 px of
 * the content column, on the same tone, so the atmosphere behind the
 * Listening Canvas runs up to the window edge. Search keeps its pill
 * because a field needs an edge; everything else is bare glyphs.
 */
export function FeTopBar({
  collapsed,
  onToggleSidebar,
}: {
  collapsed: boolean;
  onToggleSidebar: () => void;
}) {
  const { copy } = useFe();
  return (
    <header className="relative z-30 flex h-[3.25rem] shrink-0 items-center gap-3 px-6">
      <div className="flex items-center gap-0.5">
        <button
          type="button"
          className="fe-icon-btn h-8 w-8"
          aria-label={copy.topbar.back}
        >
          <ChevronLeft size={18} strokeWidth={1.75} />
        </button>
        <button
          type="button"
          className="fe-icon-btn h-8 w-8 opacity-40"
          aria-label={copy.topbar.forward}
          disabled
        >
          <ChevronRight size={18} strokeWidth={1.75} />
        </button>
        <button
          type="button"
          className="fe-icon-btn ml-1 h-8 w-8"
          aria-label={copy.topbar.sidebar}
          aria-expanded={!collapsed}
          onClick={onToggleSidebar}
        >
          {collapsed ? (
            <PanelLeftOpen size={17} strokeWidth={1.6} />
          ) : (
            <PanelLeftClose size={17} strokeWidth={1.6} />
          )}
        </button>
      </div>

      <label className="relative ml-2 flex h-9 w-[22rem] max-w-[40vw] items-center rounded-full bg-white/[0.05] pl-9 pr-4 ring-1 ring-white/[0.06] focus-within:ring-[var(--fe-accent)]">
        <Search
          size={15}
          strokeWidth={1.75}
          className="fe-ink-3 absolute left-3.5"
          aria-hidden="true"
        />
        <input
          type="search"
          placeholder={copy.topbar.search}
          className="w-full bg-transparent text-[0.8125rem] text-[var(--fe-ink)] placeholder:text-[var(--fe-ink-3)] focus:outline-none"
        />
      </label>

      <div className="ml-auto flex items-center gap-1">
        <span className="fe-label mr-3 hidden rounded-full px-2.5 py-1 ring-1 ring-white/[0.08] lg:inline-block">
          {copy.proto.badge}
        </span>
        <button
          type="button"
          className="fe-icon-btn"
          aria-label={copy.topbar.theme}
        >
          <Moon size={17} strokeWidth={1.6} />
        </button>
        <button
          type="button"
          className="ml-1 grid h-8 w-8 place-items-center rounded-full text-[0.75rem] font-semibold"
          style={{
            backgroundColor: "var(--fe-accent)",
            color: "var(--fe-accent-ink)",
          }}
          aria-label={copy.topbar.profile}
        >
          P
        </button>
      </div>
    </header>
  );
}

import type { NavItemProps } from "../../types";

export function NavItem({
  icon,
  customIcon,
  label,
  subtext,
  active,
  onClick,
  collapsed,
}: NavItemProps) {
  return (
    <button
      // Landmarks for the skins that recompose the sidebar: the item
      // and its state, so a stylesheet never has to read a utility class.
      data-nav-item
      data-active={active ? "" : undefined}
      onClick={onClick}
      title={collapsed ? label : undefined}
      className={`w-full flex items-center py-2 rounded-xl transition-colors ${
        collapsed ? "justify-center px-0" : "space-x-3 px-3"
      }
        ${
          active
            ? "bg-emerald-50 text-emerald-600 font-medium dark:bg-zinc-800 dark:text-emerald-400"
            : "hover:bg-zinc-100 hover:text-zinc-900 dark:hover:bg-zinc-800/50 dark:hover:text-zinc-200"
        }`}
    >
      {customIcon || (
        <span
          className={`transition-colors ${active ? "text-emerald-500" : "text-zinc-400"}`}
        >
          {icon}
        </span>
      )}
      {/* Kept, not dropped, when collapsed: the skins recognise a nav
          item by this column, and it is still the button's name. */}
      <div className={`flex flex-col text-left ${collapsed ? "sr-only" : ""}`}>
        <span className={`text-sm ${active ? "font-medium" : ""}`}>
          {label}
        </span>
        {subtext && (
          <span className="text-[10px] text-zinc-400">{subtext}</span>
        )}
      </div>
    </button>
  );
}

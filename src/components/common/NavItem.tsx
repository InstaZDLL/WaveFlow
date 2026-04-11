import type { NavItemProps } from "../../types";

export function NavItem({
  icon,
  customIcon,
  label,
  subtext,
  active,
  onClick,
}: NavItemProps) {
  return (
    <button
      onClick={onClick}
      className={`w-full flex items-center space-x-3 px-3 py-2 rounded-xl transition-colors
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
      <div className="flex flex-col text-left">
        <span className={`text-sm ${active ? "font-medium" : ""}`}>
          {label}
        </span>
        {subtext && <span className="text-[10px] text-zinc-400">{subtext}</span>}
      </div>
    </button>
  );
}

import type { MenuActionItemProps } from "../../types";

export function MenuActionItem({
  icon,
  label,
  danger,
  onClick,
}: MenuActionItemProps) {
  return (
    <button
      onClick={onClick}
      className={`w-full flex items-center space-x-3 px-4 py-2 text-sm transition-colors
        ${
          danger
            ? "text-red-500 hover:bg-red-50 dark:hover:bg-red-500/10"
            : "text-zinc-700 hover:bg-zinc-50 dark:text-zinc-300 dark:hover:bg-zinc-700/50"
        }`}
    >
      <span className={danger ? "text-red-400" : "text-zinc-400"}>
        {icon}
      </span>
      <span>{label}</span>
    </button>
  );
}

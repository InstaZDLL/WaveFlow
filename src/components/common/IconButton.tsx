import type { IconButtonProps } from "../../types";

export function IconButton({ icon, className, onClick }: IconButtonProps) {
  return (
    <button
      onClick={onClick}
      className={`p-2 rounded-lg transition-colors ${
        className ||
        "hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white"
      }`}
    >
      {icon}
    </button>
  );
}

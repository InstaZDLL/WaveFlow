import type { ActionLinkProps } from "../../types";

export function ActionLink({ icon, label, highlight, onClick }: ActionLinkProps) {
  return (
    <button
      onClick={onClick}
      className={`flex items-center space-x-2 text-sm font-medium transition-colors
        ${
          highlight
            ? "text-emerald-600 hover:text-emerald-700 dark:text-emerald-400 dark:hover:text-emerald-300"
            : "text-zinc-500 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-zinc-200"
        }`}
    >
      {icon}
      <span>{label}</span>
    </button>
  );
}

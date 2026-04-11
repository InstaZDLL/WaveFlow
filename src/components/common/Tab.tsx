import type { TabProps } from "../../types";

export function Tab({ icon, label, active, onClick }: TabProps) {
  return (
    <button
      onClick={onClick}
      className={`flex items-center space-x-2 pb-4 border-b-2 font-medium text-sm transition-colors
        ${
          active
            ? "border-emerald-500 text-emerald-600 dark:text-emerald-400"
            : "border-transparent text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-300"
        }`}
    >
      {icon}
      <span>{label}</span>
    </button>
  );
}

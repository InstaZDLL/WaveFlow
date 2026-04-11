import type { StatCardAccent, StatCardProps } from "../../types";

const ACCENT_CLASSES: Record<StatCardAccent, string> = {
  emerald:
    "bg-emerald-100 text-emerald-600 dark:bg-emerald-950/60 dark:text-emerald-400",
  pink: "bg-pink-100 text-pink-500 dark:bg-pink-950/60 dark:text-pink-400",
  blue: "bg-blue-100 text-blue-500 dark:bg-blue-950/60 dark:text-blue-400",
  purple:
    "bg-purple-100 text-purple-500 dark:bg-purple-950/60 dark:text-purple-400",
};

export function StatCard({
  icon,
  accent,
  count,
  label,
  onClick,
}: StatCardProps) {
  const baseClasses =
    "p-4 rounded-2xl border flex items-center space-x-4 shadow-sm transition-all bg-white border-zinc-100 dark:bg-zinc-800/40 dark:border-zinc-700/50";
  const interactiveClasses = onClick
    ? "cursor-pointer text-left w-full hover:shadow-md hover:border-emerald-200 dark:hover:border-emerald-500/40 dark:hover:bg-zinc-800 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 active:scale-[0.98]"
    : "hover:shadow-md dark:hover:bg-zinc-800";

  const content = (
    <>
      <div
        className={`w-12 h-12 rounded-xl flex items-center justify-center ${ACCENT_CLASSES[accent]}`}
      >
        {icon}
      </div>
      <div>
        <div className="text-xl font-bold text-zinc-900 dark:text-white">
          {count}
        </div>
        <div className="text-xs text-zinc-500 font-medium">{label}</div>
      </div>
    </>
  );

  if (onClick) {
    return (
      <button
        type="button"
        onClick={onClick}
        className={`${baseClasses} ${interactiveClasses}`}
      >
        {content}
      </button>
    );
  }

  return (
    <div className={`${baseClasses} ${interactiveClasses}`}>{content}</div>
  );
}

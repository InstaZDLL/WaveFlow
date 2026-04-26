import type { ReactNode } from "react";

interface TopListProps {
  title: string;
  emptyText: string;
  children: ReactNode;
}

export function TopList({ title, emptyText, children }: TopListProps) {
  const hasItems = Array.isArray(children) ? children.length > 0 : !!children;
  return (
    <section className="rounded-2xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900/60 p-5">
      <h2 className="text-sm font-semibold uppercase tracking-wide text-zinc-500 dark:text-zinc-400 mb-4">
        {title}
      </h2>
      {hasItems ? (
        <ol className="space-y-2">{children}</ol>
      ) : (
        <p className="text-sm text-zinc-500 dark:text-zinc-400 py-6 text-center">
          {emptyText}
        </p>
      )}
    </section>
  );
}

interface TopRowProps {
  rank: number;
  artwork: ReactNode;
  primary: ReactNode;
  secondary?: ReactNode;
  /** Right-aligned metric (e.g. "42 plays"). */
  metric: string;
  onClick?: () => void;
}

export function TopRow({
  rank,
  artwork,
  primary,
  secondary,
  metric,
  onClick,
}: TopRowProps) {
  const interactive = !!onClick;
  return (
    <li>
      <button
        type="button"
        onClick={onClick}
        disabled={!interactive}
        className={`w-full flex items-center gap-3 p-2 rounded-lg text-left transition-colors ${
          interactive
            ? "hover:bg-zinc-100 dark:hover:bg-zinc-800/60 cursor-pointer"
            : "cursor-default"
        }`}
      >
        <span className="w-5 text-xs font-semibold text-zinc-400 dark:text-zinc-500 tabular-nums text-right">
          {rank}
        </span>
        {artwork}
        <div className="flex-1 min-w-0">
          <div className="text-sm font-medium text-zinc-900 dark:text-white truncate">
            {primary}
          </div>
          {secondary ? (
            <div className="text-xs text-zinc-500 dark:text-zinc-400 truncate">
              {secondary}
            </div>
          ) : null}
        </div>
        <span className="text-xs text-zinc-500 dark:text-zinc-400 tabular-nums whitespace-nowrap">
          {metric}
        </span>
      </button>
    </li>
  );
}

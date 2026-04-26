import type { ReactNode } from "react";

interface KpiCardProps {
  icon: ReactNode;
  label: string;
  value: string;
  hint?: string;
}

export function KpiCard({ icon, label, value, hint }: KpiCardProps) {
  return (
    <div className="rounded-2xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900/60 p-5 flex flex-col gap-3">
      <div className="flex items-center gap-2 text-emerald-600 dark:text-emerald-400">
        {icon}
        <span className="text-xs font-medium uppercase tracking-wide text-zinc-500 dark:text-zinc-400">
          {label}
        </span>
      </div>
      <div className="text-3xl font-semibold text-zinc-900 dark:text-white tabular-nums">
        {value}
      </div>
      {hint ? (
        <div className="text-xs text-zinc-500 dark:text-zinc-400">{hint}</div>
      ) : null}
    </div>
  );
}

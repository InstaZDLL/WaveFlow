import type { ReactNode } from "react";

/**
 * The recurring WaveFlow mark: a short flowing line, in the artwork's
 * accent, where the old Home drew a 4 px coloured underline. Drawn once
 * as a path so every section header shares the same curve.
 */
export function FlowLine({ className = "w-10 h-2.5" }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 40 10"
      fill="none"
      aria-hidden="true"
      className={`fe-accent ${className}`}
      preserveAspectRatio="none"
    >
      <path
        d="M0.5 5 C 5 0.5, 9 0.5, 13.5 5 S 22 9.5, 26.5 5 S 35 0.5, 39.5 5"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
      />
    </svg>
  );
}

interface SectionHeaderProps {
  title: string;
  /** One quiet sentence under the title; most sections do without. */
  lede?: string;
  action?: ReactNode;
  className?: string;
}

export function SectionHeader({
  title,
  lede,
  action,
  className = "",
}: SectionHeaderProps) {
  return (
    <header className={`flex items-end justify-between gap-6 ${className}`}>
      <div className="min-w-0">
        <h2 className="fe-title text-[1.5rem]">{title}</h2>
        <div className="mt-2 flex items-center gap-3">
          <FlowLine />
          {lede && (
            <p className="fe-ink-3 text-[0.8125rem] leading-snug">{lede}</p>
          )}
        </div>
      </div>
      {action && <div className="shrink-0 pb-1">{action}</div>}
    </header>
  );
}

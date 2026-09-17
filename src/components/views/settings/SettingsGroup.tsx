import { useEffect, useRef, type ReactNode } from "react";
import { ChevronDown } from "lucide-react";

interface SettingsGroupProps {
  id: string;
  title: string;
  advanced?: boolean;
  reveal?: boolean;
  children: ReactNode;
}

export function SettingsGroup({
  id,
  title,
  advanced,
  reveal,
  children,
}: SettingsGroupProps) {
  const detailsRef = useRef<HTMLDetailsElement>(null);

  useEffect(() => {
    if (reveal && detailsRef.current) detailsRef.current.open = true;
  }, [reveal]);

  const heading = (
    <h3 className="text-sm font-semibold text-zinc-800 dark:text-zinc-100">
      {title}
    </h3>
  );
  const content = <div className="settings-group-body">{children}</div>;

  if (advanced) {
    return (
      <details
        ref={detailsRef}
        id={`settings-group-${id}`}
        tabIndex={-1}
        className="settings-group group scroll-mt-6 rounded-xl border border-zinc-200 dark:border-zinc-800 focus-visible:outline-emerald-500"
      >
        <summary className="flex cursor-pointer list-none items-center justify-between gap-4 rounded-xl px-4 py-4 focus-visible:outline-emerald-500 [&::-webkit-details-marker]:hidden">
          {heading}
          <ChevronDown
            size={16}
            aria-hidden="true"
            className="shrink-0 text-zinc-400 transition-transform group-open:rotate-180"
          />
        </summary>
        {content}
      </details>
    );
  }

  return (
    <section
      id={`settings-group-${id}`}
      aria-labelledby={`settings-group-heading-${id}`}
      tabIndex={-1}
      className="settings-group scroll-mt-6 focus-visible:outline-emerald-500"
    >
      <div
        id={`settings-group-heading-${id}`}
        className="border-b border-zinc-200 px-4 pb-3 dark:border-zinc-800"
      >
        {heading}
      </div>
      {content}
    </section>
  );
}

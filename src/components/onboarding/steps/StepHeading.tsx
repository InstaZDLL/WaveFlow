/** A screen's title and its one line of explanation. */
export function StepHeading({ title, lede }: { title: string; lede?: string }) {
  return (
    <div>
      <h2
        id="onboarding-title"
        className="onboarding-title text-3xl font-bold tracking-tight text-zinc-900 dark:text-white"
      >
        {title}
      </h2>
      {lede && (
        <p className="onboarding-lede mt-3 max-w-2xl text-sm leading-relaxed text-zinc-600 dark:text-zinc-400">
          {lede}
        </p>
      )}
    </div>
  );
}

/** A group of cards under a small label. */
export function StepSection({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <section className="mt-7">
      <h3 className="onboarding-kicker mb-3 text-[11px] font-semibold uppercase tracking-widest text-zinc-400">
        {label}
      </h3>
      {children}
    </section>
  );
}

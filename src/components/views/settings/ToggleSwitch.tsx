export function ToggleSwitch({
  enabled,
  onToggle,
  label,
  disabled = false,
}: {
  enabled: boolean;
  onToggle: () => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      disabled={disabled}
      role="switch"
      aria-checked={enabled}
      aria-label={label}
      className={`relative w-12 h-7 rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 focus-visible:ring-offset-2 dark:focus-visible:ring-offset-zinc-900 disabled:opacity-50 disabled:cursor-not-allowed ${
        enabled ? "bg-emerald-500" : "bg-zinc-300 dark:bg-zinc-600"
      }`}
    >
      <div
        className={`absolute top-0.5 w-6 h-6 rounded-full bg-white shadow-sm transition-transform ${
          enabled ? "left-[calc(100%-1.625rem)]" : "left-0.5"
        }`}
      />
    </button>
  );
}

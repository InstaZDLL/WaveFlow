import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Lightbulb, X } from "lucide-react";

/**
 * A one-time hint in the place it is about, with at most one action.
 * Plain utilities, so every skin paints it as its own surface.
 */
export function ContextTip({
  title,
  body,
  action,
  onDismiss,
}: {
  title: string;
  body: string;
  action?: { label: string; onClick: () => void };
  onDismiss: () => void;
}): ReactNode {
  const { t } = useTranslation();
  return (
    <div
      role="note"
      className="relative flex items-start gap-3 rounded-2xl border border-emerald-500/30 bg-emerald-500/10 p-3 pe-9 text-start"
    >
      <Lightbulb
        size={16}
        className="mt-0.5 shrink-0 text-emerald-500"
        aria-hidden="true"
      />
      <div className="min-w-0">
        <p className="text-xs font-semibold text-zinc-800 dark:text-zinc-100">
          {title}
        </p>
        <p className="mt-0.5 text-xs leading-relaxed text-zinc-600 dark:text-zinc-300">
          {body}
        </p>
        {action && (
          <button
            type="button"
            onClick={action.onClick}
            className="mt-2 text-xs font-semibold text-emerald-600 hover:underline dark:text-emerald-400"
          >
            {action.label}
          </button>
        )}
      </div>
      <button
        type="button"
        onClick={onDismiss}
        aria-label={t("tips.dismiss")}
        className="absolute end-2 top-2 rounded-full p-1 text-zinc-400 transition-colors hover:text-zinc-700 dark:hover:text-zinc-200"
      >
        <X size={13} />
      </button>
    </div>
  );
}

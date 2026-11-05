import { useId, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Loader2 } from "lucide-react";

import { useModalA11y } from "../../hooks/useModalA11y";
import {
  AnimatedModalContent,
  AnimatedModalShell,
} from "../common/AnimatedModalShell";

interface PodcastDialogShellProps {
  isOpen: boolean;
  title: string;
  /** Marks the dialog, for what has to tell them apart. */
  mark: string;
  onClose: () => void;
  /** Absent for a dialog that only reports: its one button then closes it. */
  confirm?: {
    label: string;
    danger?: boolean;
    busy: boolean;
    onConfirm: () => void;
  };
  closeLabel?: string;
  children: ReactNode;
}

/**
 * The podcast dialogs' frame. Cancel comes first in the DOM, and nothing in the
 * body takes the focus, so `useModalA11y`, which focuses the first control,
 * starts on it: Enter on an opening dialog must not accept anything. The shell
 * portals itself to the body.
 */
export function PodcastDialogShell({
  isOpen,
  title,
  mark,
  onClose,
  confirm,
  closeLabel,
  children,
}: PodcastDialogShellProps) {
  const { t } = useTranslation();
  const busy = confirm?.busy ?? false;
  const close = () => {
    if (!busy) onClose();
  };
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, close);
  const titleId = useId();
  const bodyId = useId();

  return (
    <AnimatedModalShell isOpen={isOpen} onBackdropClick={close}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={bodyId}
        data-podcast-dialog={mark}
        className="flex max-h-[calc(100vh-2rem)] w-full max-w-lg flex-col overflow-hidden rounded-2xl border border-zinc-200 bg-white shadow-2xl dark:border-zinc-800 dark:bg-zinc-900"
      >
        <div className="min-h-0 space-y-4 overflow-y-auto p-6">
          <h2
            id={titleId}
            className="text-lg font-semibold text-zinc-900 dark:text-white"
          >
            {title}
          </h2>
          <div id={bodyId} className="space-y-3">
            {children}
          </div>
        </div>
        <div className="flex shrink-0 flex-wrap items-center justify-end gap-2 border-t border-zinc-200 bg-zinc-50 px-6 py-4 dark:border-zinc-800 dark:bg-zinc-950/40">
          <button
            type="button"
            onClick={close}
            disabled={busy}
            className="rounded-lg px-4 py-2 text-sm font-medium text-zinc-700 transition-colors hover:bg-zinc-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-60 dark:text-zinc-300 dark:hover:bg-zinc-800"
          >
            {closeLabel ?? t("podcasts.dialog.cancel")}
          </button>
          {confirm && (
            <button
              type="button"
              onClick={confirm.onConfirm}
              disabled={busy}
              className={`flex items-center gap-2 rounded-lg px-4 py-2 text-sm font-medium text-white transition-colors focus:outline-none focus-visible:ring-2 disabled:cursor-not-allowed disabled:opacity-60 ${
                confirm.danger
                  ? "bg-red-600 hover:bg-red-700 focus-visible:ring-red-500"
                  : "bg-emerald-500 hover:bg-emerald-600 focus-visible:ring-emerald-500"
              }`}
            >
              {busy && (
                <Loader2
                  size={14}
                  className="animate-spin"
                  aria-hidden="true"
                />
              )}
              {confirm.label}
            </button>
          )}
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}

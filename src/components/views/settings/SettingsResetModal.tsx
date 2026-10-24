import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, RotateCcw } from "lucide-react";

import { useModalA11y } from "../../../hooks/useModalA11y";
import {
  AnimatedModalContent,
  AnimatedModalShell,
} from "../../common/AnimatedModalShell";
import type { ModifiedSetting } from "./settingsReset";

interface SettingsResetModalProps {
  isOpen: boolean;
  /** The category's own name, as its header shows it. */
  categoryLabel: string;
  /** What differs on the page, as the header counted it. */
  items: ModifiedSetting[];
  total: number;
  onClose: () => void;
  /** Runs the reset. Resolves with how many settings could not be put
   *  back; zero closes the dialog. */
  onConfirm: () => Promise<number>;
  /** Failures to show from the start: the dialog reopened after a reset
   *  that put the rest back and remounted the page. */
  initialFailed?: number;
}

/**
 * Confirms putting a Settings page back to its defaults, and says what
 * that touches: the settings that differ, by the names their rows show.
 * Nothing else changes, which the dialog says too — the things a user
 * might fear losing (accounts, folders, extensions, backups) are not
 * preferences and are never part of a page reset.
 */
export function SettingsResetModal({
  isOpen,
  categoryLabel,
  items,
  total,
  onClose,
  onConfirm,
  initialFailed = 0,
}: SettingsResetModalProps) {
  const { t } = useTranslation();
  const [submitting, setSubmitting] = useState(false);
  const [failed, setFailed] = useState(0);
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, () => {
    if (submitting) return;
    onClose();
  });
  const titleId = useId();
  const bodyId = useId();

  // Start every opening clean: a failure from the last attempt is not
  // news about this one — unless the dialog opens to report it.
  const [lastIsOpen, setLastIsOpen] = useState(isOpen);
  if (lastIsOpen !== isOpen) {
    setLastIsOpen(isOpen);
    setSubmitting(false);
    setFailed(isOpen ? initialFailed : 0);
  }

  const handleConfirm = async () => {
    if (submitting) return;
    setSubmitting(true);
    setFailed(0);
    try {
      const count = await onConfirm();
      if (count > 0) setFailed(count);
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <AnimatedModalShell
      isOpen={isOpen}
      onBackdropClick={submitting ? undefined : onClose}
    >
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={bodyId}
        data-settings-reset-dialog
        className="w-full max-w-md overflow-hidden rounded-2xl border border-zinc-200 bg-white shadow-2xl dark:border-zinc-800 dark:bg-zinc-900"
      >
        <div className="space-y-4 p-6">
          <h2
            id={titleId}
            className="text-base font-semibold text-zinc-900 dark:text-white"
          >
            {t("settings.modified.dialog.title", { category: categoryLabel })}
          </h2>
          <div id={bodyId} className="space-y-3">
            <p className="text-sm leading-5 text-zinc-600 dark:text-zinc-400">
              {t("settings.modified.dialog.body", { count: total })}
            </p>
            <ul className="settings-reset-list max-h-56 space-y-1 overflow-y-auto text-sm text-zinc-800 dark:text-zinc-200">
              {items.map((item) => (
                <li
                  key={item.labelKey}
                  className="flex items-baseline justify-between gap-3"
                >
                  <span className="min-w-0">{t(item.labelKey)}</span>
                  {item.count > 1 && (
                    <span className="settings-reset-count shrink-0 tabular-nums text-zinc-500 dark:text-zinc-400">
                      {item.count}
                    </span>
                  )}
                </li>
              ))}
            </ul>
            <p className="settings-description text-xs">
              {t("settings.modified.dialog.untouched")}
            </p>
          </div>
          {failed > 0 && (
            <div
              role="alert"
              className="rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-600 dark:border-red-500/30 dark:bg-red-500/10 dark:text-red-400"
            >
              {t("settings.modified.dialog.failed", { count: failed })}
            </div>
          )}
        </div>

        <div className="flex items-center justify-end gap-2 border-t border-zinc-200 bg-zinc-50 px-6 py-4 dark:border-zinc-800 dark:bg-zinc-950/40">
          <button
            type="button"
            onClick={onClose}
            disabled={submitting}
            className="rounded-lg px-4 py-2 text-sm font-medium text-zinc-700 transition-colors hover:bg-zinc-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-60 dark:text-zinc-300 dark:hover:bg-zinc-800"
          >
            {t("settings.modified.dialog.cancel")}
          </button>
          <button
            type="button"
            onClick={handleConfirm}
            disabled={submitting}
            className="flex items-center gap-2 rounded-lg bg-emerald-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-emerald-600 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:cursor-not-allowed disabled:opacity-60"
          >
            {submitting ? (
              <Loader2 size={14} className="animate-spin" aria-hidden="true" />
            ) : (
              <RotateCcw size={14} aria-hidden="true" />
            )}
            {t("settings.modified.dialog.confirm")}
          </button>
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}

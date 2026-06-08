import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, Loader2, Trash2 } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import { AnimatedModalContent, AnimatedModalShell } from "./AnimatedModalShell";
import { resetApp } from "../../lib/tauri/library";

interface ResetAppModalProps {
  isOpen: boolean;
  onClose: () => void;
}

const CONFIRM_TOKEN = "RESET";

export function ResetAppModal({ isOpen, onClose }: ResetAppModalProps) {
  const { t } = useTranslation();
  const [typed, setTyped] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, () => {
    if (submitting) return;
    onClose();
  });
  const titleId = useId();

  // Reset the modal's transient state on every close → reopen so a
  // previous typo / error doesn't leak into the next session. The
  // adjust-state-on-prop-change pattern (vs `useEffect` + `setState`)
  // keeps the next render frame consistent with `isOpen`'s new value
  // and avoids the cascading-render warning React 19 raises for
  // stateful effects.
  const [lastIsOpen, setLastIsOpen] = useState(isOpen);
  if (lastIsOpen !== isOpen) {
    setLastIsOpen(isOpen);
    if (!isOpen) {
      setTyped("");
      setError(null);
      setSubmitting(false);
    }
  }

  const canConfirm = typed.trim().toUpperCase() === CONFIRM_TOKEN && !submitting;

  const handleConfirm = async () => {
    if (!canConfirm) return;
    setSubmitting(true);
    setError(null);
    try {
      await resetApp();
    } catch (err) {
      setError(
        err instanceof Error
          ? err.message
          : t(
              "settings.reset.confirm.error",
              "Failed to reset. Close any external program holding a WaveFlow file (an open music player, file explorer in the data folder) and try again.",
            ),
      );
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
        className="w-full max-w-md rounded-2xl bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 shadow-2xl overflow-hidden"
      >
        <div className="p-6 space-y-4">
          <div className="flex items-start gap-3">
            <div className="shrink-0 w-10 h-10 rounded-full bg-red-100 dark:bg-red-500/15 flex items-center justify-center">
              <AlertTriangle
                size={20}
                className="text-red-600 dark:text-red-400"
                aria-hidden="true"
              />
            </div>
            <div className="flex-1 min-w-0">
              <h2
                id={titleId}
                className="text-base font-semibold text-zinc-900 dark:text-white"
              >
                {t("settings.reset.confirm.title", "Reset the entire app?")}
              </h2>
              <p className="mt-1 text-sm leading-5 text-zinc-600 dark:text-zinc-400">
                {t(
                  "settings.reset.confirm.warning",
                  "This deletes every profile, library, playlist, rating, listening history and cached cover. The app will restart into a clean onboarding. This cannot be undone.",
                )}
              </p>
            </div>
          </div>

          <div>
            <label
              htmlFor={`${titleId}-input`}
              className="block text-xs font-medium text-zinc-700 dark:text-zinc-300"
            >
              {t(
                "settings.reset.confirm.typeToConfirm",
                "Type RESET to confirm",
              )}
            </label>
            <input
              id={`${titleId}-input`}
              type="text"
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              disabled={submitting}
              autoComplete="off"
              spellCheck={false}
              className="mt-1 w-full px-3 py-2 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800 text-zinc-900 dark:text-white placeholder-zinc-400 focus:outline-none focus-visible:ring-2 focus-visible:ring-red-500 disabled:opacity-60"
              placeholder={CONFIRM_TOKEN}
            />
          </div>

          {error && (
            <div
              role="alert"
              className="text-sm text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-500/10 border border-red-200 dark:border-red-500/30 rounded-lg px-3 py-2"
            >
              {error}
            </div>
          )}
        </div>

        <div className="px-6 py-4 bg-zinc-50 dark:bg-zinc-950/40 border-t border-zinc-200 dark:border-zinc-800 flex items-center justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            disabled={submitting}
            className="px-4 py-2 text-sm font-medium rounded-lg text-zinc-700 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 disabled:opacity-60 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
          >
            {t("settings.reset.confirm.cancel", "Cancel")}
          </button>
          <button
            type="button"
            onClick={handleConfirm}
            disabled={!canConfirm}
            className="flex items-center gap-2 px-4 py-2 text-sm font-medium rounded-lg bg-red-600 hover:bg-red-700 text-white disabled:bg-red-600/40 disabled:cursor-not-allowed transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-red-500"
          >
            {submitting ? (
              <Loader2 size={14} className="animate-spin" aria-hidden="true" />
            ) : (
              <Trash2 size={14} aria-hidden="true" />
            )}
            <span>
              {t("settings.reset.confirm.confirmButton", "Reset and restart")}
            </span>
          </button>
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}

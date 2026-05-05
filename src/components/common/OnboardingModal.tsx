import { useState } from "react";
import { useTranslation } from "react-i18next";
import { FolderPlus, Loader2, CheckCircle2, Music, X } from "lucide-react";
import { useLibrary } from "../../hooks/useLibrary";
import { pickFolder } from "../../lib/tauri/dialog";
import type { ScanSummary } from "../../lib/tauri/library";

interface OnboardingModalProps {
  /** Lets the user dismiss without finishing — they can still add a
   *  folder later from Settings, and the modal will reappear on the
   *  next launch if the library is still empty. */
  onSkip: () => void;
}

type Step =
  | { kind: "welcome" }
  | { kind: "scanning"; path: string }
  | { kind: "done"; summary: ScanSummary; path: string }
  | { kind: "error"; message: string };

/**
 * First-run onboarding. Shown by `AppLayout` whenever the active
 * profile has no libraries with at least one folder. Walks the user
 * through picking a music folder and kicking off the initial scan,
 * then steps out of the way.
 */
export function OnboardingModal({ onSkip }: OnboardingModalProps) {
  const { t } = useTranslation();
  const { libraries, createLibrary, importFolder } = useLibrary();
  const [step, setStep] = useState<Step>({ kind: "welcome" });

  const handlePickFolder = async () => {
    let path: string | null;
    try {
      path = await pickFolder();
    } catch (err) {
      // User-cancelled returns null; this catch is for genuine errors.
      setStep({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
      return;
    }
    if (!path) return; // user cancelled the OS picker

    setStep({ kind: "scanning", path });

    try {
      // The active profile starts with no library at all — bootstrap
      // creates a profile, but library/library_folder rows are user-
      // driven. Provision a default library on the fly so the rest of
      // the app (which keys everything off `library_id`) has something
      // to attach folders to.
      let targetLibraryId = libraries[0]?.id;
      if (targetLibraryId == null) {
        const created = await createLibrary({
          name: t("onboarding.defaultLibraryName"),
        });
        targetLibraryId = created.id;
      }
      const summary = await importFolder(targetLibraryId, path);
      setStep({ kind: "done", summary, path });
    } catch (err) {
      setStep({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  };

  return (
    <div className="fixed inset-0 z-100 bg-black/80 flex items-center justify-center animate-fade-in p-4">
      <div className="relative w-full max-w-lg rounded-2xl bg-white dark:bg-zinc-900 shadow-2xl border border-zinc-200 dark:border-zinc-800 p-8">
        {step.kind === "welcome" && (
          <button
            type="button"
            onClick={onSkip}
            aria-label={t("common.close")}
            className="absolute top-4 right-4 p-2 rounded-full text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
          >
            <X size={20} />
          </button>
        )}

        {step.kind === "welcome" && (
          <>
            <div className="flex items-center justify-center w-16 h-16 rounded-2xl bg-emerald-500/10 mx-auto mb-5">
              <Music size={32} className="text-emerald-500" />
            </div>
            <h1 className="text-center text-2xl font-bold text-zinc-900 dark:text-zinc-100">
              {t("onboarding.welcome.title")}
            </h1>
            <p className="mt-3 text-center text-sm text-zinc-600 dark:text-zinc-400 leading-relaxed">
              {t("onboarding.welcome.message")}
            </p>
            <button
              type="button"
              onClick={handlePickFolder}
              className="mt-8 w-full inline-flex items-center justify-center gap-2 px-4 py-3 rounded-xl bg-emerald-500 text-white font-semibold hover:bg-emerald-600 transition-colors"
            >
              <FolderPlus size={18} />
              {t("onboarding.welcome.action")}
            </button>
            <button
              type="button"
              onClick={onSkip}
              className="mt-2 w-full text-center text-xs text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300 py-2 transition-colors"
            >
              {t("onboarding.welcome.skip")}
            </button>
          </>
        )}

        {step.kind === "scanning" && (
          <div className="py-8 flex flex-col items-center">
            <Loader2
              size={40}
              className="text-emerald-500 animate-spin mb-5"
            />
            <h2 className="text-lg font-semibold text-zinc-900 dark:text-zinc-100">
              {t("onboarding.scanning.title")}
            </h2>
            <p className="mt-2 text-sm text-zinc-500 dark:text-zinc-400 text-center break-all max-w-full">
              {step.path}
            </p>
          </div>
        )}

        {step.kind === "done" && (
          <>
            <div className="flex items-center justify-center w-16 h-16 rounded-2xl bg-emerald-500/10 mx-auto mb-5">
              <CheckCircle2 size={32} className="text-emerald-500" />
            </div>
            <h2 className="text-center text-xl font-bold text-zinc-900 dark:text-zinc-100">
              {t("onboarding.done.title", {
                count: step.summary.added,
              })}
            </h2>
            <p className="mt-3 text-center text-sm text-zinc-600 dark:text-zinc-400">
              {t("onboarding.done.message")}
            </p>
            <button
              type="button"
              onClick={onSkip}
              className="mt-8 w-full inline-flex items-center justify-center px-4 py-3 rounded-xl bg-emerald-500 text-white font-semibold hover:bg-emerald-600 transition-colors"
            >
              {t("onboarding.done.action")}
            </button>
          </>
        )}

        {step.kind === "error" && (
          <>
            <h2 className="text-xl font-bold text-zinc-900 dark:text-zinc-100">
              {t("onboarding.error.title")}
            </h2>
            <p className="mt-3 text-sm text-rose-500 wrap-break-word">
              {step.message}
            </p>
            <div className="mt-6 flex gap-2">
              <button
                type="button"
                onClick={() => setStep({ kind: "welcome" })}
                className="flex-1 px-4 py-2.5 rounded-xl bg-emerald-500 text-white font-medium hover:bg-emerald-600 transition-colors"
              >
                {t("onboarding.error.retry")}
              </button>
              <button
                type="button"
                onClick={onSkip}
                className="px-4 py-2.5 rounded-xl text-zinc-600 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
              >
                {t("common.close")}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

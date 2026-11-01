import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { FolderOpen, Loader2, MousePointerClick } from "lucide-react";
import { pickFolder } from "../../../lib/tauri/dialog";
import type { OnboardingScan } from "../useOnboardingScan";
import { StepHeading } from "./StepHeading";

/**
 * The music folder. Picking one registers it and moves on: the scan runs
 * behind the rest of the journey. A drop on the window goes through the
 * layout's import (the files are copied into the folder WaveFlow keeps
 * under the user's music) and moves on the same way, as soon as it
 * starts.
 */
export function FolderStep({
  startFolder,
  scan,
  onStarted,
}: {
  startFolder: (path: string) => Promise<string | null>;
  scan: OnboardingScan;
  onStarted: () => void;
}) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // A drop started from this screen: move on as the picker does. Only a
  // change seen here counts, not the state the screen was opened with.
  const initialKind = useRef(scan.kind);
  useEffect(() => {
    if (scan.kind === initialKind.current) return;
    if (scan.kind === "preparing" && !busy) onStarted();
    if (scan.kind === "error") {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setError(scan.message);
    }
  }, [scan, busy, onStarted]);

  const handlePick = async () => {
    setError(null);
    let path: string | null;
    try {
      path = await pickFolder();
    } catch (err) {
      console.error("[Onboarding] pickFolder failed", err);
      return;
    }
    if (!path) return;
    setBusy(true);
    const refused = await startFolder(path);
    setBusy(false);
    if (refused) setError(refused);
    else onStarted();
  };

  return (
    <div className="mx-auto max-w-xl py-4">
      <StepHeading
        title={t("onboarding.folder.title")}
        lede={t("onboarding.folder.lede")}
      />
      <button
        type="button"
        onClick={() => void handlePick()}
        disabled={busy}
        className="onboarding-drop group mt-8 flex w-full flex-col items-center gap-3 rounded-3xl border-2 border-dashed border-zinc-300 px-6 py-10 text-center transition-colors hover:border-emerald-500/60 hover:bg-emerald-500/5 disabled:cursor-wait dark:border-zinc-700"
      >
        {busy ? (
          <Loader2 size={30} className="animate-spin text-emerald-500" />
        ) : (
          <FolderOpen
            size={30}
            className="text-zinc-400 transition-colors group-hover:text-emerald-500"
            aria-hidden="true"
          />
        )}
        <span className="text-base font-semibold text-zinc-900 dark:text-white">
          {t("onboarding.folder.pick")}
        </span>
        <span className="flex items-center gap-1.5 text-xs text-zinc-500 dark:text-zinc-400">
          <MousePointerClick size={13} aria-hidden="true" />
          {t("onboarding.folder.drop")}
        </span>
      </button>
      {error && (
        <p className="mt-3 text-xs text-rose-500" role="alert">
          {error}
        </p>
      )}
      <p className="mt-4 text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">
        {t("onboarding.folder.hint")}
      </p>
    </div>
  );
}

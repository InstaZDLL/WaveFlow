import { useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Library,
  Loader2,
  MousePointerClick,
  Play,
  Settings2,
  Sparkles,
} from "lucide-react";
import { usePlayer } from "../../../hooks/usePlayer";
import { useLibrary } from "../../../hooks/useLibrary";
import { shuffledLibrary } from "../../discovery/shuffledLibrary";
import { openView, startPlayerTour } from "../../../lib/discovery";
import type { OnboardingScan } from "../useOnboardingScan";
import { StepHeading } from "./StepHeading";

/**
 * The end of the journey: play something, or open the library. Playing
 * shuffles what the library holds so far — with a scan still running,
 * that is what it has read until now, and the rest arrives meanwhile.
 */
export function DoneStep({
  scan,
  offerTour,
  onClose,
}: {
  scan: OnboardingScan;
  /** Offer the player bar tour: not seen yet, or the journey run again. */
  offerTour: boolean;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const { playTracks } = usePlayer();
  const { libraries } = useLibrary();
  const [starting, setStarting] = useState(false);
  const [empty, setEmpty] = useState(false);
  const [tourDeclined, setTourDeclined] = useState(false);

  const scanning = scan.kind === "preparing" || scan.kind === "running";
  const hasMusic =
    scanning ||
    scan.kind === "done" ||
    libraries.some((l) => l.folder_count > 0);

  const play = async () => {
    setStarting(true);
    try {
      const tracks = await shuffledLibrary();
      if (tracks.length === 0) {
        setEmpty(true);
        return;
      }
      await playTracks(tracks, 0, { type: "library", id: null });
      onClose();
    } catch (err) {
      console.error("[Onboarding] play failed", err);
    } finally {
      setStarting(false);
    }
  };

  return (
    <div className="mx-auto max-w-xl py-6 text-center">
      <div className="mx-auto mb-6 flex h-14 w-14 items-center justify-center rounded-full bg-emerald-500/15">
        <Sparkles size={26} className="text-emerald-500" aria-hidden="true" />
      </div>
      <StepHeading
        title={t("onboarding.done.title")}
        lede={
          scanning
            ? t("onboarding.done.ledeScanning")
            : scan.kind === "error"
              ? t("onboarding.done.ledeFailed")
              : hasMusic
                ? t("onboarding.done.lede")
                : t("onboarding.done.ledeEmpty")
        }
      />
      <p className="mt-3 text-xs text-zinc-500 dark:text-zinc-400">
        {t("onboarding.done.firstSteps")}
      </p>
      {empty && (
        <p
          className="mt-4 text-xs text-amber-600 dark:text-amber-300"
          role="status"
        >
          {t("onboarding.done.nothingYet")}
        </p>
      )}
      <div className="mt-8 flex flex-wrap items-center justify-center gap-2">
        {hasMusic ? (
          <>
            <button
              type="button"
              onClick={() => {
                openView({ view: "library" });
                onClose();
              }}
              className="onboarding-ghost inline-flex items-center gap-2 rounded-full border border-zinc-200 px-5 py-2.5 text-sm text-zinc-700 transition-colors hover:text-zinc-900 dark:border-zinc-700 dark:text-zinc-200 dark:hover:text-white"
            >
              <Library size={16} aria-hidden="true" />
              {t("onboarding.done.library")}
            </button>
            <button
              type="button"
              onClick={() => void play()}
              disabled={starting}
              className="onboarding-primary inline-flex items-center gap-2 rounded-full bg-emerald-500 px-5 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-emerald-600 disabled:opacity-60"
            >
              {starting ? (
                <Loader2 size={16} className="animate-spin" />
              ) : (
                <Play size={16} className="fill-current" aria-hidden="true" />
              )}
              {t("onboarding.done.play")}
            </button>
          </>
        ) : (
          <button
            type="button"
            onClick={onClose}
            className="onboarding-primary inline-flex items-center gap-2 rounded-full bg-emerald-500 px-5 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-emerald-600"
          >
            {t("onboarding.done.finish")}
          </button>
        )}
      </div>

      <div className="mt-8 grid gap-3 text-left sm:grid-cols-2">
        {offerTour && !tourDeclined && (
          <div className="onboarding-tile flex flex-col rounded-2xl border border-zinc-200 p-4 dark:border-zinc-800">
            <p className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white">
              <MousePointerClick
                size={16}
                className="shrink-0 text-emerald-500"
                aria-hidden="true"
              />
              {t("onboarding.done.tourTitle")}
            </p>
            <p className="mt-1 flex-1 text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">
              {t("onboarding.done.tourBody")}
            </p>
            <div className="mt-3 flex gap-2">
              <button
                type="button"
                onClick={() => {
                  // The layout starts the tour once the journey is gone.
                  startPlayerTour();
                  onClose();
                }}
                className="onboarding-primary rounded-full bg-emerald-500 px-3 py-1.5 text-xs font-semibold text-white transition-colors hover:bg-emerald-600"
              >
                {t("onboarding.done.tourStart")}
              </button>
              <button
                type="button"
                onClick={() => setTourDeclined(true)}
                className="onboarding-ghost rounded-full px-3 py-1.5 text-xs text-zinc-500 transition-colors hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-white"
              >
                {t("onboarding.done.tourLater")}
              </button>
            </div>
          </div>
        )}
        <div
          className={`onboarding-tile flex flex-col rounded-2xl border border-zinc-200 p-4 dark:border-zinc-800 ${
            offerTour && !tourDeclined ? "" : "sm:col-span-2"
          }`}
        >
          <p className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white">
            <Settings2
              size={16}
              className="shrink-0 text-emerald-500"
              aria-hidden="true"
            />
            {t("onboarding.done.settingsTitle")}
          </p>
          <p className="mt-1 flex-1 text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">
            {t("onboarding.done.settingsBody")}
          </p>
          <div className="mt-3">
            <button
              type="button"
              onClick={() => {
                openView({ view: "settings" });
                onClose();
              }}
              className="onboarding-ghost rounded-full border border-zinc-200 px-3 py-1.5 text-xs font-semibold text-zinc-700 transition-colors hover:border-emerald-500 hover:text-emerald-600 dark:border-zinc-700 dark:text-zinc-200"
            >
              {t("onboarding.done.settingsOpen")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

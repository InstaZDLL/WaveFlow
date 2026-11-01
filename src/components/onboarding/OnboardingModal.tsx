import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { ChevronLeft, ChevronRight, X } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import { useLibrary } from "../../hooks/useLibrary";
import { useProfile } from "../../hooks/useProfile";
import { ScanStrip } from "./ScanStrip";
import { useOnboardingScan } from "./useOnboardingScan";
import { usePluginCatalogue } from "./usePluginCatalogue";
import { WelcomeStep } from "./steps/WelcomeStep";
import { FolderStep } from "./steps/FolderStep";
import { LookStep } from "./steps/LookStep";
import { ListeningStep } from "./steps/ListeningStep";
import { LyricsStep } from "./steps/LyricsStep";
import { PluginsStep } from "./steps/PluginsStep";
import { ConnectionsStep } from "./steps/ConnectionsStep";
import { DoneStep } from "./steps/DoneStep";
import { playerTourSeen } from "../../lib/discovery";
import "../discovery/discovery.css";

/**
 * Name the backend gives the auto-created first profile
 * ([`state.rs::create_default_profile`](src-tauri/crates/app/src/state.rs)).
 * Not localised, so the comparison is reliable: a profile made through
 * the New Profile modal already carries the name its user typed, and the
 * welcome screen does not ask for it again.
 */
const AUTO_BOOTSTRAP_PROFILE_NAME = "Default";

/** Getting started: about a minute, then the scan runs on its own. */
const SETUP_STEPS = ["welcome", "folder"] as const;
/** "Make WaveFlow yours", one theme a screen, while the library fills. */
const COMPOSE_STEPS = [
  "look",
  "listening",
  "lyrics",
  "plugins",
  "connections",
] as const;

type SetupStep = (typeof SETUP_STEPS)[number];
type ComposeStep = (typeof COMPOSE_STEPS)[number];
type StepId = SetupStep | ComposeStep | "done";

interface OnboardingModalProps {
  /** Close the journey. The layout latches `onboarding.dismissed` for the
   *  profile, so it does not come back on the next launch. */
  onClose: () => void;
  /** Run again from Settings: the player bar tour is offered at the end
   *  even when this profile has seen it. */
  replay?: boolean;
}

/**
 * The first-run journey, in three movements:
 *
 * 1. **Getting started** — welcome and language (the auto-created
 *    profile is named here too), then the music folder. Picking it, or
 *    dropping one on the window, starts the scan at once.
 * 2. **Make WaveFlow yours** — while the scan runs in the background,
 *    one screen per theme (look, listening, lyrics, plugins,
 *    connections) of cards switched on in a click. Every screen can be
 *    skipped, and "Skip all" goes to the end.
 * 3. **Done** — play something or open the library. The Home's "First
 *    steps" card takes over from there.
 *
 * Every setting goes through the path Settings uses for it; the journey
 * writes nothing of its own. Run again from Settings → General, it skips
 * the folder when the library already has one.
 */
export function OnboardingModal({
  onClose,
  replay = false,
}: OnboardingModalProps) {
  const { t } = useTranslation();
  const { libraries } = useLibrary();
  const { activeProfile, renameProfile } = useProfile();
  const { scan, startFolder } = useOnboardingScan();

  // Both decided once, at mount: a rename or the first scan would
  // otherwise change the list of steps under the index.
  const [askName] = useState(
    () => activeProfile?.name === AUTO_BOOTSTRAP_PROFILE_NAME,
  );
  const [hasLibrary] = useState(() =>
    libraries.some((l) => l.folder_count > 0),
  );
  const steps: StepId[] = [
    "welcome",
    ...(hasLibrary ? [] : (["folder"] as const)),
    ...COMPOSE_STEPS,
    "done",
  ];

  const [index, setIndex] = useState(0);
  const step = steps[index];
  // The store is read on the screens that show it, not before.
  const plugins = usePluginCatalogue(step === "lyrics" || step === "plugins");
  // The screens the user changed something on: their primary button
  // reads "Continue" rather than "Skip".
  const [touched, setTouched] = useState<ReadonlySet<StepId>>(new Set());
  const touch = useCallback(() => {
    setTouched((cur) => (cur.has(step) ? cur : new Set(cur).add(step)));
  }, [step]);

  // The player bar tour is offered at the end when this profile has not
  // seen it, or when the journey was asked for again.
  const [tourSeen, setTourSeen] = useState(true);
  const profileId = activeProfile?.id ?? null;
  useEffect(() => {
    if (profileId == null) return;
    let cancelled = false;
    playerTourSeen(profileId)
      .then((seen) => {
        if (!cancelled) setTourSeen(seen);
      })
      .catch((err) => console.error("[Onboarding] read tour flag failed", err));
    return () => {
      cancelled = true;
    };
  }, [profileId]);

  const [profileName, setProfileName] = useState(activeProfile?.name ?? "");
  const [nameError, setNameError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const dialogRef = useModalA11y<HTMLDivElement>(true, onClose);
  const bodyRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    bodyRef.current?.scrollTo({ top: 0, behavior: "auto" });
  }, [step]);

  const next = useCallback(
    () => setIndex((i) => Math.min(steps.length - 1, i + 1)),
    [steps.length],
  );
  const back = () => setIndex((i) => Math.max(0, i - 1));
  const skipAll = () => setIndex(steps.length - 1);

  const handleStart = async () => {
    if (askName && activeProfile) {
      const trimmed = profileName.trim();
      if (!trimmed) {
        setNameError(t("onboarding.welcome.nameRequired"));
        return;
      }
      if (trimmed !== activeProfile.name) {
        setBusy(true);
        try {
          await renameProfile(activeProfile.id, trimmed);
        } catch (err) {
          console.error("[Onboarding] rename profile failed", err);
          setNameError(err instanceof Error ? err.message : String(err));
          setBusy(false);
          return;
        }
        setBusy(false);
      }
    }
    next();
  };

  const composeIndex = COMPOSE_STEPS.indexOf(step as ComposeStep);
  const setupIndex = SETUP_STEPS.indexOf(step as SetupStep);
  const setupCount = hasLibrary ? 1 : SETUP_STEPS.length;
  const kicker =
    composeIndex >= 0
      ? t("onboarding.phase.compose", {
          current: composeIndex + 1,
          total: COMPOSE_STEPS.length,
        })
      : setupIndex >= 0
        ? t("onboarding.phase.setup", {
            current: setupIndex + 1,
            total: setupCount,
          })
        : null;

  return createPortal(
    <div className="fixed inset-0 z-100 flex items-center justify-center bg-black/70 p-4 backdrop-blur-md animate-fade-in">
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="onboarding-title"
        className="onboarding-shell relative flex max-h-[calc(100vh-2rem)] w-full max-w-3xl flex-col overflow-hidden rounded-3xl border border-zinc-200 bg-white shadow-2xl dark:border-zinc-800 dark:bg-zinc-950"
      >
        <div className="flex shrink-0 items-center gap-4 px-8 pt-6">
          <div className="min-w-0 flex-1">
            {kicker && (
              <p className="onboarding-kicker text-[11px] font-semibold uppercase tracking-widest text-zinc-400">
                {kicker}
              </p>
            )}
            {composeIndex >= 0 && (
              <div className="mt-2 flex gap-1.5" aria-hidden="true">
                {COMPOSE_STEPS.map((s, i) => (
                  <span
                    key={s}
                    data-on={i <= composeIndex || undefined}
                    className={`onboarding-dot h-1 w-6 rounded-full ${
                      i <= composeIndex
                        ? "bg-emerald-500"
                        : "bg-zinc-200 dark:bg-zinc-800"
                    }`}
                  />
                ))}
              </div>
            )}
          </div>
          {step !== "welcome" && step !== "folder" && <ScanStrip scan={scan} />}
          <button
            type="button"
            onClick={onClose}
            aria-label={t("onboarding.actions.close")}
            className="onboarding-ghost rounded-full p-2 text-zinc-400 transition-colors hover:bg-zinc-100 hover:text-zinc-700 dark:hover:bg-zinc-800 dark:hover:text-zinc-200"
          >
            <X size={18} />
          </button>
        </div>

        <div
          ref={bodyRef}
          className="min-h-0 flex-1 overflow-y-auto px-8 pb-2 pt-5"
        >
          <div key={step} className="onboarding-step">
            {step === "welcome" && (
              <WelcomeStep
                askName={askName}
                profileName={profileName}
                onProfileNameChange={(value) => {
                  setProfileName(value);
                  if (nameError) setNameError(null);
                }}
                nameError={nameError}
              />
            )}
            {step === "folder" && (
              <FolderStep
                startFolder={startFolder}
                scan={scan}
                onStarted={next}
              />
            )}
            {step === "look" && <LookStep onTouch={touch} />}
            {step === "listening" && <ListeningStep onTouch={touch} />}
            {step === "lyrics" && (
              <LyricsStep onTouch={touch} plugins={plugins} />
            )}
            {step === "plugins" && (
              <PluginsStep onTouch={touch} plugins={plugins} />
            )}
            {step === "connections" && (
              <ConnectionsStep
                onTouch={touch}
                onLeave={onClose}
                storeUnreachable={plugins.catalogue.status === "unavailable"}
              />
            )}
            {step === "done" && (
              <DoneStep
                scan={scan}
                offerTour={replay || !tourSeen}
                onClose={onClose}
              />
            )}
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-2 px-8 pb-7 pt-4">
          {step === "welcome" && (
            <>
              <button
                type="button"
                onClick={onClose}
                className="onboarding-ghost rounded-full border border-zinc-200 px-4 py-2.5 text-sm text-zinc-600 transition-colors hover:text-zinc-900 dark:border-zinc-700 dark:text-zinc-300 dark:hover:text-white"
              >
                {t("onboarding.actions.later")}
              </button>
              <span className="flex-1" />
              <PrimaryButton
                onClick={() => void handleStart()}
                disabled={busy || (askName && !profileName.trim())}
                label={t("onboarding.actions.start")}
              />
            </>
          )}
          {step === "folder" && (
            <>
              <BackButton onClick={back} />
              <span className="flex-1" />
              <button
                type="button"
                onClick={next}
                className="onboarding-ghost rounded-full border border-zinc-200 px-4 py-2.5 text-sm text-zinc-600 transition-colors hover:text-zinc-900 dark:border-zinc-700 dark:text-zinc-300 dark:hover:text-white"
              >
                {t("onboarding.actions.folderLater")}
              </button>
            </>
          )}
          {composeIndex >= 0 && (
            <>
              <BackButton onClick={back} />
              <span className="flex-1" />
              <button
                type="button"
                onClick={skipAll}
                className="onboarding-ghost rounded-full px-4 py-2.5 text-sm text-zinc-500 transition-colors hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-white"
              >
                {t("onboarding.actions.skipAll")}
              </button>
              <PrimaryButton
                onClick={next}
                label={
                  touched.has(step)
                    ? t("onboarding.actions.continue")
                    : t("onboarding.actions.skip")
                }
              />
            </>
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}

function PrimaryButton({
  onClick,
  label,
  disabled,
}: {
  onClick: () => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="onboarding-primary inline-flex items-center gap-1.5 rounded-full bg-emerald-500 px-5 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-emerald-600 disabled:cursor-not-allowed disabled:opacity-50"
    >
      {label}
      <ChevronRight size={16} className="rtl:rotate-180" />
    </button>
  );
}

function BackButton({ onClick }: { onClick: () => void }) {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      onClick={onClick}
      className="onboarding-ghost inline-flex items-center gap-1 rounded-full px-3 py-2.5 text-sm text-zinc-500 transition-colors hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-white"
    >
      <ChevronLeft size={16} className="rtl:rotate-180" />
      {t("onboarding.actions.back")}
    </button>
  );
}

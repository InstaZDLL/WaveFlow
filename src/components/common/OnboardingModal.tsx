import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { AnimatePresence, motion } from "framer-motion";
import {
  AlertCircle,
  AudioLines,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Disc3,
  Eye,
  EyeOff,
  ExternalLink,
  FolderOpen,
  Loader2,
  Mic2,
  Music,
  Play,
  SkipForward,
  Sparkles,
  X,
  Zap,
} from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useModalA11y } from "../../hooks/useModalA11y";
import { useLibrary } from "../../hooks/useLibrary";
import { pickFolder } from "../../lib/tauri/dialog";
import type { ScanSummary } from "../../lib/tauri/library";
import {
  getAutoAnalyze,
  setAutoAnalyze,
} from "../../lib/tauri/analysis";
import {
  getDiscordRpcEnabled,
  getLastfmApiKey,
  getLastfmApiSecret,
  lastfmGetStatus,
  lastfmLogin,
  setDiscordRpcEnabled,
  setLastfmApiKey,
  setLastfmApiSecret,
  type LastfmStatus,
} from "../../lib/tauri/integration";

interface OnboardingModalProps {
  /** Closes the wizard. Persists `onboarding.dismissed=true` on the
   *  parent side so the modal doesn't reappear next launch. */
  onSkip: () => void;
}

type StepId =
  | "welcome"
  | "localOnly"
  | "folder"
  | "lastfm"
  | "integrations"
  | "scan"
  | "done";

const STEPS: ReadonlyArray<StepId> = [
  "welcome",
  "localOnly",
  "folder",
  "lastfm",
  "integrations",
  "scan",
  "done",
];

const STEP_ICONS: Record<StepId, typeof Music> = {
  welcome: Music,
  localOnly: AlertCircle,
  folder: FolderOpen,
  lastfm: AudioLines,
  integrations: Zap,
  scan: Disc3,
  done: Sparkles,
};

type ScanState =
  | { kind: "idle" }
  | { kind: "running"; path: string }
  | { kind: "done"; summary: ScanSummary; path: string }
  | { kind: "error"; message: string };

/**
 * Multi-step first-run wizard. Inspired by Lokal's onboarding flow,
 * adapted to WaveFlow's feature set:
 *
 *   1. welcome      — branding + skip-or-start
 *   2. localOnly    — set expectation that WaveFlow is not a streaming service
 *   3. folder       — pick music folder + auto-analyze toggle
 *   4. lastfm       — recommended scrobbling/bio integration (the user asked
 *                     for this to be highlighted)
 *   5. integrations — Discord Rich Presence opt-in
 *   6. scan         — confirm + kick off the initial library scan
 *   7. done         — celebrate + start listening
 *
 * Shown by `AppLayout` once per profile (latched via
 * `onboarding.dismissed` profile setting). The wizard never persists
 * its own progress: closing mid-flow means the user starts from
 * step 1 next time, which is the desired UX for a 30-second flow.
 */
export function OnboardingModal({ onSkip }: OnboardingModalProps) {
  const { t } = useTranslation();
  const { libraries, createLibrary, importFolder } = useLibrary();

  const [stepIndex, setStepIndex] = useState(0);
  const stepId = STEPS[stepIndex];

  // Folder + scan state shared across steps.
  const [musicFolder, setMusicFolder] = useState<string | null>(null);
  const [scanState, setScanState] = useState<ScanState>({ kind: "idle" });

  // Quick settings on the folder step. Hydrated from the backend at
  // mount so toggles already reflect any preference set elsewhere
  // (rare on first run, but the wizard CAN be reopened in dev).
  const [autoAnalyze, setAutoAnalyzeState] = useState(true);

  // Last.fm form state. The backend wants api key + secret + username
  // + password to mint a session via auth.getMobileSession; we mirror
  // the SettingsView shape so this step is essentially a compact
  // version of the Integrations panel.
  const [lastfmKey, setLastfmKey] = useState("");
  const [lastfmSecret, setLastfmSecret] = useState("");
  const [lastfmSecretVisible, setLastfmSecretVisible] = useState(false);
  const [lastfmUsername, setLastfmUsername] = useState("");
  const [lastfmPassword, setLastfmPassword] = useState("");
  const [lastfmPasswordVisible, setLastfmPasswordVisible] = useState(false);
  const [lastfmStatus, setLastfmStatus] = useState<LastfmStatus | null>(null);
  const [lastfmBusy, setLastfmBusy] = useState(false);
  const [lastfmError, setLastfmError] = useState<string | null>(null);

  // Discord opt-in on the integrations step.
  const [discordEnabled, setDiscordEnabled] = useState(false);
  const [discordBusy, setDiscordBusy] = useState(false);

  // Modal a11y wiring. The wizard is "open" for its entire lifetime
  // (the parent unmounts it when the user clicks Skip / Start), so
  // Escape funnels to onSkip and we always trap focus.
  const dialogRef = useModalA11y<HTMLDivElement>(true, onSkip);

  // Hydrate stored preferences once on mount. Failures are swallowed
  // because they only affect the initial toggle state — saving a
  // toggle later still works either way.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const value = await getAutoAnalyze();
        if (!cancelled) setAutoAnalyzeState(value);
      } catch (err) {
        console.error("[Onboarding] read auto_analyze failed", err);
      }
      try {
        const key = await getLastfmApiKey();
        if (!cancelled && key) setLastfmKey(key);
      } catch (err) {
        console.error("[Onboarding] read lastfm api key failed", err);
      }
      try {
        const secret = await getLastfmApiSecret();
        if (!cancelled && secret) setLastfmSecret(secret);
      } catch (err) {
        console.error("[Onboarding] read lastfm api secret failed", err);
      }
      try {
        const status = await lastfmGetStatus();
        if (!cancelled) setLastfmStatus(status);
      } catch (err) {
        console.error("[Onboarding] read lastfm status failed", err);
      }
      try {
        const enabled = await getDiscordRpcEnabled();
        if (!cancelled) setDiscordEnabled(enabled);
      } catch (err) {
        console.error("[Onboarding] read discord rpc failed", err);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const Icon = STEP_ICONS[stepId];

  const goNext = () => setStepIndex((i) => Math.min(STEPS.length - 1, i + 1));
  const goBack = () => setStepIndex((i) => Math.max(0, i - 1));

  // === Folder step actions ============================================
  const handlePickFolder = async () => {
    let path: string | null;
    try {
      path = await pickFolder();
    } catch (err) {
      console.error("[Onboarding] pickFolder failed", err);
      return;
    }
    if (!path) return;
    setMusicFolder(path);
  };

  const handleToggleAutoAnalyze = async (next: boolean) => {
    setAutoAnalyzeState(next);
    try {
      await setAutoAnalyze(next);
    } catch (err) {
      console.error("[Onboarding] set auto_analyze failed", err);
      // Roll back so the toggle reflects truth.
      setAutoAnalyzeState(!next);
    }
  };

  // === Last.fm step actions ===========================================
  const handleOpenLastfmKeyPage = async () => {
    try {
      await openUrl("https://www.last.fm/api/account/create");
    } catch (err) {
      console.error("[Onboarding] open lastfm key page failed", err);
    }
  };

  const handleLastfmLogin = async () => {
    const trimmedKey = lastfmKey.trim();
    const trimmedSecret = lastfmSecret.trim();
    const trimmedUser = lastfmUsername.trim();
    if (
      !trimmedKey ||
      !trimmedSecret ||
      !trimmedUser ||
      !lastfmPassword
    ) {
      setLastfmError(t("onboarding.lastfm.missingFields"));
      return;
    }
    setLastfmBusy(true);
    setLastfmError(null);
    try {
      // Persist the key + secret first so the backend has them
      // available when it composes the signed login request.
      await setLastfmApiKey(trimmedKey);
      await setLastfmApiSecret(trimmedSecret);
      const status = await lastfmLogin(trimmedUser, lastfmPassword);
      setLastfmStatus(status);
      setLastfmPassword("");
    } catch (err) {
      console.error("[Onboarding] lastfm login failed", err);
      setLastfmError(err instanceof Error ? err.message : String(err));
    } finally {
      setLastfmBusy(false);
    }
  };

  // === Integrations step actions ======================================
  const handleToggleDiscord = async (next: boolean) => {
    setDiscordBusy(true);
    try {
      await setDiscordRpcEnabled(next);
      setDiscordEnabled(next);
    } catch (err) {
      console.error("[Onboarding] set discord rpc failed", err);
    } finally {
      setDiscordBusy(false);
    }
  };

  // === Scan step actions ==============================================
  const handleScanNow = async () => {
    if (!musicFolder) return;
    setScanState({ kind: "running", path: musicFolder });
    try {
      let libraryId = libraries[0]?.id;
      if (libraryId == null) {
        const created = await createLibrary({
          name: t("onboarding.defaultLibraryName"),
        });
        libraryId = created.id;
      }
      const summary = await importFolder(libraryId, musicFolder);
      setScanState({ kind: "done", summary, path: musicFolder });
      goNext();
    } catch (err) {
      setScanState({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  };

  // Progress bar segments — one per step, filled up to (and including)
  // the active step. We never animate beyond the current index so
  // back-navigation visually dims the trailing bars.
  const progress = useMemo(
    () => STEPS.map((_, i) => i <= stepIndex),
    [stepIndex],
  );

  const isLastStep = stepIndex === STEPS.length - 1;
  const showCloseButton = stepIndex === 0;

  return (
    <motion.div
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      transition={{ duration: 0.18, ease: "easeOut" }}
      className="fixed inset-0 z-100 bg-black/80 backdrop-blur-md flex items-center justify-center p-4"
    >
      <motion.div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="onboarding-title"
        initial={{ opacity: 0, scale: 0.95, y: 8 }}
        animate={{ opacity: 1, scale: 1, y: 0 }}
        transition={{ type: "spring", stiffness: 380, damping: 28, mass: 0.6 }}
        className="relative w-full max-w-lg rounded-3xl bg-white dark:bg-zinc-900 shadow-2xl border border-zinc-200 dark:border-zinc-800 overflow-hidden"
      >
        {/* Close button — only available on the very first step so the
            user can't half-onboard themselves into a broken state. */}
        {showCloseButton && (
          <button
            type="button"
            onClick={onSkip}
            aria-label={t("common.close")}
            className="absolute top-4 right-4 z-10 p-2 rounded-full text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
          >
            <X size={18} />
          </button>
        )}

        {/* Progress bar */}
        <div className="flex items-center gap-1.5 p-4 pb-0">
          {progress.map((isFilled, i) => (
            <div
              key={i}
              className={`h-1 flex-1 rounded-full transition-colors duration-300 ${
                isFilled
                  ? "bg-emerald-500"
                  : "bg-zinc-200 dark:bg-zinc-800"
              }`}
            />
          ))}
        </div>

        <div className="px-8 pt-6">
          <AnimatePresence mode="wait">
            <motion.div
              key={stepId}
              initial={{ opacity: 0, x: 16 }}
              animate={{ opacity: 1, x: 0 }}
              exit={{ opacity: 0, x: -16 }}
              transition={{ duration: 0.2, ease: "easeOut" }}
            >
              <div className="flex justify-center mb-5">
                <div className="p-3.5 bg-emerald-500/10 rounded-2xl">
                  <Icon size={32} className="text-emerald-500" />
                </div>
              </div>
              <h2
                id="onboarding-title"
                className="text-center text-2xl font-bold text-zinc-900 dark:text-white"
              >
                {t(`onboarding.${stepId}.title`)}
              </h2>
              <p className="mt-3 text-center text-sm text-zinc-600 dark:text-zinc-400 leading-relaxed">
                {t(`onboarding.${stepId}.description`)}
              </p>

              {/* === Step bodies ====================================== */}

              {stepId === "localOnly" && (
                <div className="mt-6 rounded-xl border border-amber-500/30 bg-amber-500/10 p-4">
                  <div className="flex items-start gap-3">
                    <AlertCircle
                      size={18}
                      className="text-amber-500 shrink-0 mt-0.5"
                      aria-hidden="true"
                    />
                    <div className="space-y-2 text-xs text-amber-700 dark:text-amber-200/90 leading-relaxed">
                      <p className="font-semibold">
                        {t("onboarding.localOnly.calloutTitle")}
                      </p>
                      <ul className="list-disc list-inside space-y-1">
                        <li>{t("onboarding.localOnly.bulletOwn")}</li>
                        <li>{t("onboarding.localOnly.bulletImport")}</li>
                        <li>{t("onboarding.localOnly.bulletScrobble")}</li>
                      </ul>
                    </div>
                  </div>
                </div>
              )}

              {stepId === "folder" && (
                <div className="mt-6 space-y-4">
                  <button
                    type="button"
                    onClick={handlePickFolder}
                    className="w-full rounded-xl border-2 border-dashed border-zinc-300 dark:border-zinc-700 px-4 py-5 text-center cursor-pointer hover:border-emerald-500/50 hover:bg-emerald-500/5 transition-all group"
                  >
                    <FolderOpen
                      size={22}
                      className={`mx-auto mb-2 transition-colors ${
                        musicFolder
                          ? "text-emerald-500"
                          : "text-zinc-400 group-hover:text-emerald-500"
                      }`}
                      aria-hidden="true"
                    />
                    {musicFolder ? (
                      <>
                        <p className="text-sm text-zinc-900 dark:text-zinc-100 break-all px-2">
                          {musicFolder}
                        </p>
                        <p className="mt-1 text-xs text-zinc-500">
                          {t("onboarding.folder.changeHint")}
                        </p>
                      </>
                    ) : (
                      <p className="text-sm text-zinc-500 dark:text-zinc-400 group-hover:text-zinc-900 dark:group-hover:text-white transition-colors">
                        {t("onboarding.folder.placeholder")}
                      </p>
                    )}
                  </button>

                  <div className="rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-800/40 p-4 space-y-3">
                    <p className="text-[10px] font-bold tracking-widest uppercase text-zinc-400">
                      {t("onboarding.folder.quickSettings")}
                    </p>
                    <ToggleRow
                      label={t("onboarding.folder.autoAnalyze.title")}
                      description={t("onboarding.folder.autoAnalyze.description")}
                      value={autoAnalyze}
                      onChange={handleToggleAutoAnalyze}
                    />
                  </div>
                </div>
              )}

              {stepId === "lastfm" && (
                <div className="mt-6 space-y-4">
                  {/* "Recommended" pill so the step visually stands out
                      from the other optionals, per the user's "highlight
                      Last.fm" directive. */}
                  <div className="flex items-center justify-center gap-2">
                    <span className="inline-flex items-center gap-1.5 px-2.5 py-0.5 rounded-full text-[10px] font-bold tracking-wider uppercase bg-emerald-500/15 text-emerald-600 dark:text-emerald-300 border border-emerald-500/30">
                      <Mic2 size={11} />
                      {t("onboarding.lastfm.recommendedBadge")}
                    </span>
                  </div>

                  {lastfmStatus?.connected ? (
                    <div className="rounded-xl border border-emerald-500/30 bg-emerald-500/10 p-4 flex items-start gap-3">
                      <CheckCircle2
                        size={18}
                        className="text-emerald-500 shrink-0 mt-0.5"
                        aria-hidden="true"
                      />
                      <div className="text-sm">
                        <p className="font-semibold text-emerald-700 dark:text-emerald-300">
                          {t("onboarding.lastfm.connectedTitle", {
                            user: lastfmStatus.username ?? "",
                          })}
                        </p>
                        <p className="mt-1 text-xs text-emerald-700/80 dark:text-emerald-200/80">
                          {t("onboarding.lastfm.connectedSubtitle")}
                        </p>
                      </div>
                    </div>
                  ) : (
                    <div className="rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-800/40 p-4 space-y-3">
                      <button
                        type="button"
                        onClick={handleOpenLastfmKeyPage}
                        className="w-full inline-flex items-center justify-center gap-2 text-xs font-medium text-emerald-600 dark:text-emerald-400 hover:text-emerald-700 dark:hover:text-emerald-300 transition-colors"
                      >
                        {t("onboarding.lastfm.keyHint")}
                        <ExternalLink size={12} aria-hidden="true" />
                      </button>
                      <Input
                        label={t("onboarding.lastfm.keyLabel")}
                        value={lastfmKey}
                        onChange={setLastfmKey}
                        placeholder={t("onboarding.lastfm.keyPlaceholder")}
                      />
                      <Input
                        label={t("onboarding.lastfm.secretLabel")}
                        value={lastfmSecret}
                        onChange={setLastfmSecret}
                        placeholder={t("onboarding.lastfm.secretPlaceholder")}
                        type={lastfmSecretVisible ? "text" : "password"}
                        rightSlot={
                          <VisibilityToggle
                            visible={lastfmSecretVisible}
                            onToggle={() =>
                              setLastfmSecretVisible((v) => !v)
                            }
                            ariaLabel={t("onboarding.lastfm.toggleSecret")}
                          />
                        }
                      />
                      <Input
                        label={t("onboarding.lastfm.userLabel")}
                        value={lastfmUsername}
                        onChange={setLastfmUsername}
                        placeholder={t("onboarding.lastfm.userPlaceholder")}
                      />
                      <Input
                        label={t("onboarding.lastfm.passwordLabel")}
                        value={lastfmPassword}
                        onChange={setLastfmPassword}
                        placeholder={t("onboarding.lastfm.passwordPlaceholder")}
                        type={lastfmPasswordVisible ? "text" : "password"}
                        rightSlot={
                          <VisibilityToggle
                            visible={lastfmPasswordVisible}
                            onToggle={() =>
                              setLastfmPasswordVisible((v) => !v)
                            }
                            ariaLabel={t("onboarding.lastfm.togglePassword")}
                          />
                        }
                      />
                      {lastfmError && (
                        <p className="text-xs text-rose-500" role="alert">
                          {lastfmError}
                        </p>
                      )}
                      <button
                        type="button"
                        onClick={handleLastfmLogin}
                        disabled={lastfmBusy}
                        className="w-full inline-flex items-center justify-center gap-2 px-4 py-2.5 rounded-xl bg-emerald-500 hover:bg-emerald-600 text-white text-sm font-semibold transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                      >
                        {lastfmBusy ? (
                          <Loader2 size={16} className="animate-spin" />
                        ) : (
                          <Mic2 size={16} />
                        )}
                        {t("onboarding.lastfm.connect")}
                      </button>
                    </div>
                  )}
                </div>
              )}

              {stepId === "integrations" && (
                <div className="mt-6 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-800/40 p-4">
                  <ToggleRow
                    label={t("onboarding.integrations.discord.title")}
                    description={t(
                      "onboarding.integrations.discord.description",
                    )}
                    value={discordEnabled}
                    onChange={handleToggleDiscord}
                    busy={discordBusy}
                  />
                </div>
              )}

              {stepId === "scan" && (
                <div className="mt-6 space-y-3">
                  {scanState.kind === "running" ? (
                    <div className="flex flex-col items-center gap-3 py-2">
                      <Loader2
                        size={40}
                        className="text-emerald-500 animate-spin"
                      />
                      <p className="text-sm text-zinc-900 dark:text-zinc-100">
                        {t("onboarding.scan.scanning")}
                      </p>
                      <p className="text-xs text-zinc-500 break-all text-center">
                        {scanState.path}
                      </p>
                    </div>
                  ) : scanState.kind === "error" ? (
                    <div className="rounded-xl border border-rose-500/30 bg-rose-500/10 p-4">
                      <p className="text-sm font-semibold text-rose-700 dark:text-rose-300">
                        {t("onboarding.scan.errorTitle")}
                      </p>
                      <p className="mt-1 text-xs text-rose-700/80 dark:text-rose-200/80 break-all">
                        {scanState.message}
                      </p>
                    </div>
                  ) : (
                    <div className="rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-800/40 p-4 flex items-start gap-3">
                      <FolderOpen
                        size={18}
                        className="text-emerald-500 shrink-0 mt-0.5"
                        aria-hidden="true"
                      />
                      <div className="min-w-0">
                        <p className="text-sm text-zinc-900 dark:text-zinc-100 break-all">
                          {musicFolder ?? t("onboarding.scan.noFolder")}
                        </p>
                        <p className="mt-1 text-xs text-zinc-500">
                          {t("onboarding.scan.readyHint")}
                        </p>
                      </div>
                    </div>
                  )}
                </div>
              )}

              {stepId === "done" && (
                <div className="mt-6 space-y-4">
                  <div className="mx-auto flex items-center justify-center w-14 h-14 rounded-full bg-emerald-500/20">
                    <CheckCircle2
                      size={32}
                      className="text-emerald-500"
                      aria-hidden="true"
                    />
                  </div>
                  <div className="rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-800/40 p-4">
                    <p className="text-xs font-semibold text-zinc-700 dark:text-zinc-200">
                      {t("onboarding.done.nextSteps")}
                    </p>
                    <ul className="mt-2 space-y-1 text-xs text-zinc-600 dark:text-zinc-400 list-disc list-inside">
                      <li>{t("onboarding.done.bulletExplore")}</li>
                      <li>{t("onboarding.done.bulletQueue")}</li>
                      <li>{t("onboarding.done.bulletSettings")}</li>
                    </ul>
                  </div>
                </div>
              )}
            </motion.div>
          </AnimatePresence>
        </div>

        {/* Action bar — varies per step. Kept outside AnimatePresence so
            the buttons don't shimmer between transitions. */}
        <div className="px-8 pb-8 pt-6">
          {stepId === "welcome" && (
            <div className="flex gap-2">
              <button
                type="button"
                onClick={onSkip}
                className="flex-1 inline-flex items-center justify-center gap-2 px-4 py-3 rounded-xl border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 text-sm text-zinc-600 dark:text-zinc-300 hover:text-zinc-900 dark:hover:text-white hover:border-zinc-300 dark:hover:border-zinc-600 transition-colors"
              >
                <SkipForward size={16} />
                {t("onboarding.actions.skipSetup")}
              </button>
              <button
                type="button"
                onClick={goNext}
                className="flex-1 inline-flex items-center justify-center gap-2 px-4 py-3 rounded-xl bg-emerald-500 hover:bg-emerald-600 text-white text-sm font-semibold transition-colors"
              >
                {t("onboarding.actions.getStarted")}
                <ChevronRight size={16} />
              </button>
            </div>
          )}

          {stepId === "localOnly" && (
            <DefaultActions
              onBack={goBack}
              onNext={goNext}
              nextLabel={t("onboarding.actions.understood")}
              t={t}
            />
          )}

          {stepId === "folder" && (
            <DefaultActions
              onBack={goBack}
              onNext={goNext}
              nextDisabled={!musicFolder}
              t={t}
            />
          )}

          {stepId === "lastfm" && (
            <div className="flex gap-2">
              <button
                type="button"
                onClick={goBack}
                className="px-4 py-3 rounded-xl border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 text-sm text-zinc-600 dark:text-zinc-300 hover:text-zinc-900 dark:hover:text-white transition-colors inline-flex items-center gap-1"
              >
                <ChevronLeft size={16} />
                {t("onboarding.actions.back")}
              </button>
              <button
                type="button"
                onClick={goNext}
                className="flex-1 inline-flex items-center justify-center gap-2 px-4 py-3 rounded-xl bg-emerald-500 hover:bg-emerald-600 text-white text-sm font-semibold transition-colors"
              >
                {lastfmStatus?.connected
                  ? t("onboarding.actions.continue")
                  : t("onboarding.actions.skipForNow")}
                <ChevronRight size={16} />
              </button>
            </div>
          )}

          {stepId === "integrations" && (
            <DefaultActions onBack={goBack} onNext={goNext} t={t} />
          )}

          {stepId === "scan" && (
            <div className="flex gap-2">
              <button
                type="button"
                onClick={goNext}
                disabled={scanState.kind === "running"}
                className="flex-1 inline-flex items-center justify-center gap-2 px-4 py-3 rounded-xl border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 text-sm text-zinc-600 dark:text-zinc-300 hover:text-zinc-900 dark:hover:text-white transition-colors disabled:opacity-50"
              >
                <SkipForward size={16} />
                {t("onboarding.actions.scanLater")}
              </button>
              <button
                type="button"
                onClick={handleScanNow}
                disabled={!musicFolder || scanState.kind === "running"}
                className="flex-1 inline-flex items-center justify-center gap-2 px-4 py-3 rounded-xl bg-emerald-500 hover:bg-emerald-600 text-white text-sm font-semibold transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {scanState.kind === "running" ? (
                  <Loader2 size={16} className="animate-spin" />
                ) : (
                  <Play size={16} />
                )}
                {t("onboarding.actions.scanNow")}
              </button>
            </div>
          )}

          {isLastStep && (
            <button
              type="button"
              onClick={onSkip}
              className="w-full inline-flex items-center justify-center gap-2 px-4 py-3 rounded-xl bg-emerald-500 hover:bg-emerald-600 text-white text-sm font-semibold transition-colors"
            >
              <CheckCircle2 size={16} />
              {t("onboarding.actions.startListening")}
            </button>
          )}
        </div>
      </motion.div>
    </motion.div>
  );
}

/* ─── Helpers ──────────────────────────────────────────────────────── */

function DefaultActions({
  onBack,
  onNext,
  nextLabel,
  nextDisabled,
  t,
}: {
  onBack: () => void;
  onNext: () => void;
  nextLabel?: string;
  nextDisabled?: boolean;
  t: (key: string) => string;
}) {
  return (
    <div className="flex gap-2">
      <button
        type="button"
        onClick={onBack}
        className="px-4 py-3 rounded-xl border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 text-sm text-zinc-600 dark:text-zinc-300 hover:text-zinc-900 dark:hover:text-white transition-colors inline-flex items-center gap-1"
      >
        <ChevronLeft size={16} />
        {t("onboarding.actions.back")}
      </button>
      <button
        type="button"
        onClick={onNext}
        disabled={nextDisabled}
        className="flex-1 inline-flex items-center justify-center gap-2 px-4 py-3 rounded-xl bg-emerald-500 hover:bg-emerald-600 text-white text-sm font-semibold transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
      >
        {nextLabel ?? t("onboarding.actions.continue")}
        <ChevronRight size={16} />
      </button>
    </div>
  );
}

function ToggleRow({
  label,
  description,
  value,
  onChange,
  busy,
}: {
  label: string;
  description: string;
  value: boolean;
  onChange: (next: boolean) => void;
  busy?: boolean;
}) {
  return (
    <div className="flex items-start justify-between gap-3">
      <div className="min-w-0">
        <p className="text-sm font-medium text-zinc-900 dark:text-white">
          {label}
        </p>
        <p className="mt-0.5 text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed">
          {description}
        </p>
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={value}
        aria-label={label}
        onClick={() => onChange(!value)}
        disabled={busy}
        className={`relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 ${
          value ? "bg-emerald-500" : "bg-zinc-300 dark:bg-zinc-700"
        }`}
      >
        <span
          className={`inline-block h-4 w-4 transform rounded-full bg-white shadow transition-transform ${
            value ? "translate-x-6" : "translate-x-1"
          }`}
        />
      </button>
    </div>
  );
}

function Input({
  label,
  value,
  onChange,
  placeholder,
  type = "text",
  rightSlot,
}: {
  label: string;
  value: string;
  onChange: (next: string) => void;
  placeholder?: string;
  type?: "text" | "password";
  rightSlot?: React.ReactNode;
}) {
  return (
    <label className="block">
      <span className="block text-[10px] font-bold tracking-widest uppercase text-zinc-500 dark:text-zinc-400 mb-1">
        {label}
      </span>
      <div className="relative">
        <input
          type={type}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
          autoComplete="off"
          spellCheck={false}
          className="w-full px-3 py-2 pr-10 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-sm text-zinc-900 dark:text-zinc-100 placeholder:text-zinc-400 focus:outline-none focus:ring-2 focus:ring-emerald-500/40 focus:border-emerald-500/40"
        />
        {rightSlot && (
          <div className="absolute inset-y-0 right-2 flex items-center">
            {rightSlot}
          </div>
        )}
      </div>
    </label>
  );
}

function VisibilityToggle({
  visible,
  onToggle,
  ariaLabel,
}: {
  visible: boolean;
  onToggle: () => void;
  ariaLabel: string;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-label={ariaLabel}
      className="p-1 rounded text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors"
    >
      {visible ? <EyeOff size={14} /> : <Eye size={14} />}
    </button>
  );
}

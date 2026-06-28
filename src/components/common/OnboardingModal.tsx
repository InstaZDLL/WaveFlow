import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AnimatePresence, motion } from "framer-motion";
import {
  AlertCircle,
  AudioLines,
  Check,
  Database,
  FileText,
  FileDown,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Columns2,
  Disc3,
  Eye,
  EyeOff,
  ExternalLink,
  FolderOpen,
  Globe,
  Layers,
  Loader2,
  Mic2,
  Music,
  Palette,
  Play,
  SkipForward,
  Sparkles,
  UserRound,
  X,
} from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useModalA11y } from "../../hooks/useModalA11y";
import { useLibrary } from "../../hooks/useLibrary";
import { useProfile } from "../../hooks/useProfile";
import { useTheme } from "../../hooks/useTheme";
import { useSkin } from "../../hooks/useSkin";
import { THEME_PRESETS } from "../../lib/themes";
import { SKIN_PRESETS } from "../../lib/skins";
import { pickFolder } from "../../lib/tauri/dialog";
import type { ScanSummary } from "../../lib/tauri/library";
import {
  SUPPORTED_LANGUAGES,
  normalizeSupportedLanguageCode,
} from "../../i18n";
import { getAutoAnalyze, setAutoAnalyze } from "../../lib/tauri/analysis";
import {
  getLyricsDefaultDestination,
  setLyricsDefaultDestination,
  type LyricsDestination,
} from "../../lib/tauri/lyrics";
import {
  getLastfmApiKey,
  getLastfmApiSecret,
  lastfmGetStatus,
  lastfmLogin,
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
  | "language"
  | "profile"
  | "localOnly"
  | "appearance"
  | "lyrics"
  | "folder"
  | "lastfm"
  | "scan"
  | "done";

const STEPS: ReadonlyArray<StepId> = [
  "welcome",
  "language",
  "profile",
  "localOnly",
  "appearance",
  "lyrics",
  "folder",
  "lastfm",
  "scan",
  "done",
];

/**
 * Name the backend hardcodes for the auto-bootstrapped first profile
 * (see [`state.rs::create_default_profile`](src-tauri/src/state.rs)).
 * Not localised — a user-created profile from the "New profile" modal
 * always carries a user-supplied name, so anything other than this
 * literal means we already have the user's intent and the rename
 * step would be redundant.
 */
const AUTO_BOOTSTRAP_PROFILE_NAME = "Default";

const STEP_ICONS: Record<StepId, typeof Music> = {
  welcome: Music,
  language: Globe,
  profile: UserRound,
  localOnly: AlertCircle,
  appearance: Palette,
  lyrics: Mic2,
  folder: FolderOpen,
  lastfm: AudioLines,
  scan: Disc3,
  done: Sparkles,
};

/**
 * Reads the browser/system language and tells the caller whether
 * i18next ended up on a "real" detected language (matching the user's
 * locale) or fell back to English because the OS language isn't
 * supported. The UI uses this to show a green "Detected" badge or a
 * yellow "we couldn't match your language" fallback hint on the
 * language step.
 */
function detectInitialLanguage(): { code: string; fallback: boolean } {
  const raw =
    (typeof navigator !== "undefined"
      ? (navigator.language ?? navigator.languages?.[0])
      : null) ?? "en";
  const normalized = normalizeSupportedLanguageCode(raw);
  // normalizeSupportedLanguageCode returns the first supported code
  // ("en") when the system locale can't be matched. If we end up on
  // "en" but the user's raw locale doesn't actually start with "en",
  // that's a genuine fallback — surface it to the user.
  const isFallback = normalized === "en" && !raw.toLowerCase().startsWith("en");
  return { code: normalized, fallback: isFallback };
}

type ScanState =
  | { kind: "idle" }
  | { kind: "running"; path: string }
  | { kind: "done"; summary: ScanSummary; path: string }
  | { kind: "error"; message: string };

/**
 * Multi-step first-run wizard. Inspired by Lokal's onboarding flow,
 * adapted to WaveFlow's feature set:
 *
 *   1. welcome   — branding + skip-or-start
 *   2. language  — confirm the auto-detected UI language with a green
 *                  "Detected" badge, or surface the fallback when the
 *                  user's system locale isn't one of our 17 supported
 *                  languages (e.g. Egyptian Arabic resolves to "ar",
 *                  Greek falls back to "en" with a yellow hint)
 *   3. localOnly — set expectation that WaveFlow is not a streaming service
 *   4. folder    — pick music folder + auto-analyze toggle
 *   5. lastfm    — recommended scrobbling/bio integration (the user asked
 *                  for this to be highlighted)
 *   6. scan      — confirm + kick off the initial library scan
 *   7. done      — celebrate + start listening
 *
 * Discord Rich Presence is intentionally NOT a wizard step. It defaults
 * to ON in the backend (`app_setting['integrations.discord_rpc'] = true`)
 * so new users get the activity card by default; opt-out lives in
 * Settings → Integrations.
 *
 * Shown by `AppLayout` once per profile (latched via
 * `onboarding.dismissed` profile setting). The wizard never persists
 * its own progress: closing mid-flow means the user starts from
 * step 1 next time, which is the desired UX for a 30-second flow.
 */
export function OnboardingModal({ onSkip }: OnboardingModalProps) {
  const { t, i18n } = useTranslation();
  const { libraries, createLibrary, importFolder } = useLibrary();
  const { activeProfile, renameProfile } = useProfile();
  const { theme, setThemeId } = useTheme();
  const { skin, setSkinId } = useSkin();

  // Decide ONCE at mount whether to include the `profile` rename
  // step, then never recompute it. The parent gates this modal on a
  // resolved active profile (see [`ui.md`](../../docs/features/ui.md)),
  // so the lazy initializer always has the truthful value.
  //
  // Recomputing this on every `activeProfile` change would shrink
  // the steps array under our feet right after `renameProfile`
  // optimistically updates the context — `stepIndex` would still
  // point to the old `profile` position (now occupied by `localOnly`),
  // and the subsequent `goNext()` would skip `localOnly` entirely.
  const [includeProfileStep] = useState(
    () => activeProfile?.name === AUTO_BOOTSTRAP_PROFILE_NAME,
  );
  const steps = useMemo(
    () => (includeProfileStep ? STEPS : STEPS.filter((s) => s !== "profile")),
    [includeProfileStep],
  );

  const [stepIndex, setStepIndex] = useState(0);
  const stepId = steps[stepIndex];

  // Reset the body scroll on every step transition. The
  // `AnimatePresence` swap re-renders the content but the surrounding
  // scroll container is reused, so a user who scrolled to fill the
  // tall Last.fm step would land mid-page on the next step otherwise.
  const scrollBodyRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    scrollBodyRef.current?.scrollTo({ top: 0, behavior: "auto" });
  }, [stepId]);

  // Profile-name step state. Seeded from the active profile so the
  // input reflects whatever name the auto-bootstrapper picked
  // ("Default" on a fresh install).
  const [profileName, setProfileName] = useState("");
  const [profileBusy, setProfileBusy] = useState(false);
  const [profileError, setProfileError] = useState<string | null>(null);
  useEffect(() => {
    if (activeProfile && profileName === "") {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setProfileName(activeProfile.name);
    }
  }, [activeProfile, profileName]);

  // Folder + scan state shared across steps.
  const [musicFolder, setMusicFolder] = useState<string | null>(null);
  const [scanState, setScanState] = useState<ScanState>({ kind: "idle" });

  // Quick settings on the folder step. Hydrated from the backend at
  // mount so toggles already reflect any preference set elsewhere
  // (rare on first run, but the wizard CAN be reopened in dev).
  const [autoAnalyze, setAutoAnalyzeState] = useState(true);
  // Issue #201 — pre-flight the lyrics-write destination so users who
  // want to keep their tag block clean don't have to discover the
  // Settings card after first save. Initial render uses `tag` so the
  // grid renders before the fetch lands; the effect below pulls the
  // app-wide stored value, and a click here writes the setting
  // immediately so a wizard cancel still persists the choice.
  const [lyricsDestination, setLyricsDestinationState] =
    useState<LyricsDestination>("tag");
  const [lyricsBusy, setLyricsBusy] = useState(false);
  // True once the user has clicked a tile in the lyrics step. Lets
  // the initial fetch's then-handler defer to a manual pick that
  // landed first — same race the LyricsEditorModal hits, only here
  // the modal isn't reopened so we don't need to reset between
  // steps.
  const lyricsUserTouchedRef = useRef(false);

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

  // Initial language detection — frozen at mount so the "Detected"
  // badge keeps pointing at the user's system locale even after they
  // manually switch language inside the step. The active selection is
  // read live from i18n.resolvedLanguage further down.
  const initialDetection = useMemo(() => detectInitialLanguage(), []);
  const activeLanguageCode = normalizeSupportedLanguageCode(
    i18n.resolvedLanguage ?? i18n.language,
  );

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
        const dest = await getLyricsDefaultDestination();
        if (cancelled || lyricsUserTouchedRef.current) return;
        setLyricsDestinationState(dest);
      } catch (err) {
        console.error(
          "[Onboarding] read lyrics default destination failed",
          err,
        );
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
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const Icon = STEP_ICONS[stepId];

  const goNext = () => setStepIndex((i) => Math.min(steps.length - 1, i + 1));
  const goBack = () => setStepIndex((i) => Math.max(0, i - 1));

  // === Language step actions ==========================================
  const handlePickLanguage = (code: string) => {
    i18n.changeLanguage(code).catch((err) => {
      console.error("[Onboarding] changeLanguage failed", err);
    });
  };

  // === Profile step actions ===========================================
  const handleProfileContinue = async () => {
    const trimmed = profileName.trim();
    if (!trimmed) {
      setProfileError(t("onboarding.profile.required"));
      return;
    }
    // The modal is supposed to open only once the profile is resolved
    // (see ui.md), but guard against the race anyway — without this we
    // would silently drop the user's name and advance.
    if (!activeProfile) {
      setProfileError(t("onboarding.profile.unavailable"));
      return;
    }
    // Skip the backend round-trip when the name hasn't actually
    // changed (user accepted the seeded default).
    if (trimmed !== activeProfile.name) {
      setProfileBusy(true);
      setProfileError(null);
      try {
        await renameProfile(activeProfile.id, trimmed);
      } catch (err) {
        console.error("[Onboarding] rename profile failed", err);
        setProfileError(err instanceof Error ? err.message : String(err));
        setProfileBusy(false);
        return;
      }
      setProfileBusy(false);
    }
    goNext();
  };

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

  const handlePickLyricsDestination = async (next: LyricsDestination) => {
    if (next === lyricsDestination || lyricsBusy) return;
    lyricsUserTouchedRef.current = true;
    const previous = lyricsDestination;
    setLyricsBusy(true);
    setLyricsDestinationState(next);
    try {
      await setLyricsDefaultDestination(next);
    } catch (err) {
      console.error("[Onboarding] set lyrics default destination failed", err);
      // Roll back so the grid reflects what's actually persisted.
      setLyricsDestinationState(previous);
    } finally {
      setLyricsBusy(false);
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
    if (!trimmedKey || !trimmedSecret || !trimmedUser || !lastfmPassword) {
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
    () => steps.map((_, i) => i <= stepIndex),
    [steps, stepIndex],
  );

  const isLastStep = stepIndex === steps.length - 1;
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
        // Cap the modal at the viewport height (minus the parent's 1rem
        // padding on each side = 2rem) and lay out as a column with a
        // scrollable middle. Without this cap the wizard's tallest step
        // (Last.fm with 4 inputs + button) pushed both the progress bar
        // and the action bar off-screen on 1080p displays (#107).
        className="relative w-full max-w-lg rounded-3xl bg-white dark:bg-zinc-900 shadow-2xl border border-zinc-200 dark:border-zinc-800 overflow-hidden flex flex-col max-h-[calc(100vh-2rem)]"
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

        {/* Progress bar — sticky header inside the flex column so it
            stays visible while the body scrolls. */}
        <div className="flex items-center gap-1.5 p-4 pb-0 shrink-0">
          {progress.map((isFilled, i) => (
            <div
              key={i}
              className={`h-1 flex-1 rounded-full transition-colors duration-300 ${
                isFilled ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-800"
              }`}
            />
          ))}
        </div>

        <div
          ref={scrollBodyRef}
          className="px-8 pt-6 overflow-y-auto flex-1 min-h-0"
        >
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

              {stepId === "language" && (
                <div className="mt-6 space-y-4">
                  {initialDetection.fallback ? (
                    <div className="rounded-xl border border-amber-500/30 bg-amber-500/10 p-3 flex items-start gap-3">
                      <AlertCircle
                        size={16}
                        className="text-amber-500 shrink-0 mt-0.5"
                        aria-hidden="true"
                      />
                      <p className="text-xs text-amber-700 dark:text-amber-200/90 leading-relaxed">
                        {t("onboarding.language.fallback")}
                      </p>
                    </div>
                  ) : null}
                  <div className="grid grid-cols-2 sm:grid-cols-3 gap-2 max-h-[280px] overflow-y-auto pr-1">
                    {SUPPORTED_LANGUAGES.map((lang) => {
                      const isActive = lang.code === activeLanguageCode;
                      const isDetected =
                        !initialDetection.fallback &&
                        lang.code === initialDetection.code;
                      return (
                        <button
                          key={lang.code}
                          type="button"
                          onClick={() => handlePickLanguage(lang.code)}
                          aria-pressed={isActive}
                          className={`relative px-3 py-2.5 rounded-xl border text-sm text-left transition-all focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${
                            isActive
                              ? "border-emerald-500 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300"
                              : "border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-zinc-700 dark:text-zinc-300 hover:border-zinc-300 dark:hover:border-zinc-600"
                          }`}
                        >
                          <span className="block truncate font-medium">
                            {lang.nativeLabel}
                          </span>
                          {isDetected && (
                            <span
                              className="absolute top-1.5 right-1.5 inline-flex items-center justify-center w-4 h-4 rounded-full bg-emerald-500 text-white"
                              aria-label={t("onboarding.language.detected")}
                              title={t("onboarding.language.detected")}
                            >
                              <CheckCircle2 size={12} strokeWidth={2.5} />
                            </span>
                          )}
                        </button>
                      );
                    })}
                  </div>
                </div>
              )}

              {stepId === "profile" && (
                <div className="mt-6 space-y-3">
                  <Input
                    label={t("onboarding.profile.nameLabel")}
                    value={profileName}
                    onChange={(next) => {
                      setProfileName(next);
                      if (profileError) setProfileError(null);
                    }}
                    placeholder={t("onboarding.profile.namePlaceholder")}
                  />
                  <p className="text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed">
                    {t("onboarding.profile.hint")}
                  </p>
                  {profileError && (
                    <p className="text-xs text-rose-500" role="alert">
                      {profileError}
                    </p>
                  )}
                </div>
              )}

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

              {stepId === "appearance" && (
                <div className="mt-6 space-y-6">
                  {/* Theme picker — same visual contract as Settings →
                    Appearance but inline, no card wrapper. Each click
                    fires `setThemeId` which writes to
                    `profile_setting['appearance.theme.id']` so the
                    choice survives the onboarding-then-quit edge case. */}
                  <div className="space-y-3">
                    <div className="flex items-center gap-2">
                      <Palette
                        size={16}
                        className="text-zinc-400 shrink-0"
                        aria-hidden="true"
                      />
                      <h3 className="text-sm font-semibold text-zinc-900 dark:text-white">
                        {t("onboarding.appearance.theme.title")}
                      </h3>
                    </div>
                    <div className="grid grid-cols-3 sm:grid-cols-5 gap-2">
                      {THEME_PRESETS.map((preset) => {
                        const isActive = preset.id === theme.id;
                        return (
                          <button
                            key={preset.id}
                            type="button"
                            onClick={(event) => setThemeId(preset.id, event)}
                            aria-pressed={isActive}
                            aria-label={t(preset.labelKey)}
                            className={`group relative rounded-lg border overflow-hidden transition-all focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${
                              isActive
                                ? "border-emerald-500 ring-2 ring-emerald-500/30"
                                : "border-zinc-200 dark:border-zinc-700 hover:border-zinc-300 dark:hover:border-zinc-600"
                            }`}
                          >
                            <div
                              className="h-10 flex items-center justify-between px-2"
                              style={{
                                backgroundColor:
                                  preset.ambient ??
                                  (preset.mode === "dark"
                                    ? "#121212"
                                    : "#ffffff"),
                              }}
                            >
                              <div className="flex space-x-0.5">
                                <div
                                  className="w-2 h-2 rounded-full"
                                  style={{
                                    backgroundColor: preset.accent[400],
                                  }}
                                />
                                <div
                                  className="w-2 h-2 rounded-full"
                                  style={{
                                    backgroundColor: preset.accent[500],
                                  }}
                                />
                                <div
                                  className="w-2 h-2 rounded-full"
                                  style={{
                                    backgroundColor: preset.accent[600],
                                  }}
                                />
                              </div>
                              {isActive && (
                                <Check
                                  size={11}
                                  strokeWidth={3}
                                  style={{ color: preset.accent[500] }}
                                />
                              )}
                            </div>
                            <div className="px-2 py-1 bg-white dark:bg-zinc-900 text-left">
                              <div className="text-[10px] font-medium text-zinc-700 dark:text-zinc-200 truncate">
                                {t(preset.labelKey)}
                              </div>
                            </div>
                          </button>
                        );
                      })}
                    </div>
                  </div>

                  {/* Skin picker — orthogonal axis to theme. Click
                    fires `setSkinId` which writes
                    `profile_setting['appearance.skin.id']`. */}
                  <div className="space-y-3">
                    <div className="flex items-center gap-2">
                      <Layers
                        size={16}
                        className="text-zinc-400 shrink-0"
                        aria-hidden="true"
                      />
                      <h3 className="text-sm font-semibold text-zinc-900 dark:text-white">
                        {t("onboarding.appearance.skin.title")}
                      </h3>
                    </div>
                    <div className="grid grid-cols-2 sm:grid-cols-3 gap-2">
                      {SKIN_PRESETS.map((preset) => {
                        const isActive = preset.id === skin.id;
                        return (
                          <button
                            key={preset.id}
                            type="button"
                            onClick={() => setSkinId(preset.id)}
                            aria-pressed={isActive}
                            className={`group relative rounded-lg border px-3 py-2.5 text-left transition-all focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${
                              isActive
                                ? "border-emerald-500 ring-2 ring-emerald-500/30 bg-emerald-50 dark:bg-emerald-500/10"
                                : "border-zinc-200 dark:border-zinc-700 hover:border-zinc-300 dark:hover:border-zinc-600 bg-white dark:bg-zinc-800/50"
                            }`}
                          >
                            <div className="flex items-center justify-between gap-2">
                              <span className="text-xs font-semibold text-zinc-800 dark:text-zinc-100 truncate">
                                {t(preset.labelKey)}
                              </span>
                              {isActive && (
                                <Check
                                  size={12}
                                  strokeWidth={3}
                                  className="text-emerald-500 shrink-0"
                                />
                              )}
                            </div>
                            <span className="block text-[10px] text-zinc-500 dark:text-zinc-400 mt-0.5 truncate">
                              {t(preset.descriptionKey)}
                            </span>
                          </button>
                        );
                      })}
                    </div>
                  </div>

                  <p className="text-[11px] text-zinc-400 italic">
                    {t("onboarding.appearance.hint")}
                  </p>
                </div>
              )}

              {stepId === "lyrics" && (
                <div className="mt-6 space-y-4">
                  <div
                    role="radiogroup"
                    aria-label={t("onboarding.lyrics.title")}
                    className="grid grid-cols-1 sm:grid-cols-3 gap-2"
                  >
                    {(
                      [
                        { id: "tag", Icon: FileText },
                        { id: "sidecar", Icon: FileDown },
                        { id: "db_only", Icon: Database },
                      ] as const
                    ).map(({ id, Icon }) => {
                      const isActive = lyricsDestination === id;
                      return (
                        <button
                          key={id}
                          type="button"
                          role="radio"
                          aria-checked={isActive}
                          disabled={lyricsBusy}
                          onClick={() => void handlePickLyricsDestination(id)}
                          className={`group relative rounded-xl border p-3 text-left transition-all focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 ${
                            isActive
                              ? "border-emerald-500 ring-2 ring-emerald-500/30 bg-emerald-50 dark:bg-emerald-500/10"
                              : "border-zinc-200 dark:border-zinc-700 hover:border-zinc-300 dark:hover:border-zinc-600 bg-white dark:bg-zinc-800/50"
                          }`}
                        >
                          <div className="flex items-start justify-between gap-2">
                            <Icon
                              size={18}
                              className={
                                isActive
                                  ? "text-emerald-600 dark:text-emerald-400"
                                  : "text-zinc-400"
                              }
                              aria-hidden="true"
                            />
                            {isActive && (
                              <Check
                                size={14}
                                strokeWidth={3}
                                className="text-emerald-500"
                              />
                            )}
                          </div>
                          <div className="mt-2 text-sm font-semibold text-zinc-900 dark:text-white">
                            {t(`lyricsEditor.destination.${id}.label`)}
                          </div>
                          <div className="mt-1 text-[11px] text-zinc-500 dark:text-zinc-400 leading-snug">
                            {t(`lyricsEditor.destination.${id}.hint`)}
                          </div>
                        </button>
                      );
                    })}
                  </div>
                  <p className="text-[11px] text-zinc-400 italic">
                    {t("onboarding.lyrics.hint")}
                  </p>
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
                      description={t(
                        "onboarding.folder.autoAnalyze.description",
                      )}
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
                            onToggle={() => setLastfmSecretVisible((v) => !v)}
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
                            onToggle={() => setLastfmPasswordVisible((v) => !v)}
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
                  {/* Discreet feature tip — surfaces the immersive view
                      so new users discover it without a dedicated step.
                      A reusable callout slot for future tips. */}
                  <div className="flex items-start gap-3 rounded-xl border border-emerald-500/30 bg-emerald-500/10 p-4">
                    <Columns2
                      size={18}
                      className="text-emerald-500 mt-0.5 shrink-0"
                      aria-hidden="true"
                    />
                    <div className="min-w-0">
                      <p className="text-xs font-semibold text-zinc-700 dark:text-zinc-200">
                        {t("onboarding.done.tipImmersiveTitle")}
                      </p>
                      <p className="text-xs text-zinc-600 dark:text-zinc-400 mt-0.5 leading-relaxed">
                        {t("onboarding.done.tipImmersiveBody")}
                      </p>
                    </div>
                  </div>
                </div>
              )}
            </motion.div>
          </AnimatePresence>
        </div>

        {/* Action bar — varies per step. Kept outside AnimatePresence so
            the buttons don't shimmer between transitions. `shrink-0` +
            its position as the last flex child pin it to the modal's
            bottom edge regardless of body height. */}
        <div className="px-8 pb-8 pt-6 shrink-0">
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

          {stepId === "language" && (
            <DefaultActions onBack={goBack} onNext={goNext} t={t} />
          )}

          {stepId === "profile" && (
            <div className="flex gap-2">
              <button
                type="button"
                onClick={goBack}
                disabled={profileBusy}
                className="px-4 py-3 rounded-xl border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 text-sm text-zinc-600 dark:text-zinc-300 hover:text-zinc-900 dark:hover:text-white transition-colors inline-flex items-center gap-1 disabled:opacity-50"
              >
                <ChevronLeft size={16} />
                {t("onboarding.actions.back")}
              </button>
              <button
                type="button"
                onClick={handleProfileContinue}
                disabled={profileBusy || !profileName.trim()}
                className="flex-1 inline-flex items-center justify-center gap-2 px-4 py-3 rounded-xl bg-emerald-500 hover:bg-emerald-600 text-white text-sm font-semibold transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {profileBusy ? (
                  <Loader2 size={16} className="animate-spin" />
                ) : null}
                {t("onboarding.actions.continue")}
                {!profileBusy && <ChevronRight size={16} />}
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

          {stepId === "appearance" && (
            <DefaultActions onBack={goBack} onNext={goNext} t={t} />
          )}

          {stepId === "lyrics" && (
            <DefaultActions onBack={goBack} onNext={goNext} t={t} />
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
          className={`w-full px-3 py-2 ${rightSlot ? "pr-10" : ""} rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-sm text-zinc-900 dark:text-zinc-100 placeholder:text-zinc-400 focus:outline-none focus:ring-2 focus:ring-emerald-500/40 focus:border-emerald-500/40`}
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

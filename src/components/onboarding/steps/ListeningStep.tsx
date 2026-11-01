import { useTranslation } from "react-i18next";
import { useProfileScopedToggle } from "../../../hooks/useProfileScopedToggle";
import { SETTINGS } from "../../../lib/settingsRegistry";
import {
  playerGetAudioSettings,
  playerSetCrossfade,
  playerSetReplayGain,
} from "../../../lib/tauri/player";
import { getAutoMix, setAutoMix } from "../../../lib/tauri/smartCrossfade";
import { getAutoAnalyze, setAutoAnalyze } from "../../../lib/tauri/analysis";
import { FeatureCard } from "../../discovery/FeatureCard";
import { StepHeading } from "./StepHeading";

/**
 * The fade a "crossfade" card turns on. Settings → Playback keeps the
 * slider (0-12 s); the card is a yes or no, and a yes needs a length.
 */
const CROSSFADE_ON_SECONDS = 6;

// The reads and writes, at module level so their identity is stable.
const readCrossfade = () =>
  playerGetAudioSettings().then((s) => s.crossfade_ms > 0);
const writeCrossfade = (on: boolean, profileId: number | null) =>
  playerSetCrossfade(on ? CROSSFADE_ON_SECONDS : 0, profileId);
const readReplayGain = () => playerGetAudioSettings().then((s) => s.replaygain);
// `set_auto_analyze` has no expected-profile check: the profile the
// hook hands over cannot be passed on.
const writeAutoAnalyze = (on: boolean) => setAutoAnalyze(on);

/**
 * Listening: the transitions, the loudness and the analysis that feeds
 * the radios and AutoMix. Each card is the switch Settings has for it,
 * through the same command. The engine ones carry the profile they were
 * clicked in; the analysis command takes no profile, as in Settings, and
 * writes into the active one.
 */
export function ListeningStep({ onTouch }: { onTouch: () => void }) {
  const { t } = useTranslation();
  const crossfade = useProfileScopedToggle(readCrossfade, writeCrossfade, {
    label: "Onboarding crossfade",
    fallback: SETTINGS["audio.crossfade_ms"].defaultValue > 0,
  });
  const autoMix = useProfileScopedToggle(getAutoMix, setAutoMix, {
    label: "Onboarding automix",
    fallback: SETTINGS["audio.automix"].defaultValue,
  });
  const replayGain = useProfileScopedToggle(
    readReplayGain,
    playerSetReplayGain,
    {
      label: "Onboarding replaygain",
      fallback: SETTINGS["audio.replaygain"].defaultValue,
    },
  );
  const analysis = useProfileScopedToggle(getAutoAnalyze, writeAutoAnalyze, {
    label: "Onboarding auto analyze",
    fallback: SETTINGS["audio.auto_analyze"].defaultValue,
  });

  return (
    <div>
      <StepHeading
        title={t("onboarding.listening.title")}
        lede={t("onboarding.listening.lede")}
      />
      <div className="mt-7 grid grid-cols-1 gap-3 sm:grid-cols-2">
        <FeatureCard
          art="crossfade"
          title={t("onboarding.listening.crossfade.title")}
          benefit={t("onboarding.listening.crossfade.benefit", {
            seconds: CROSSFADE_ON_SECONDS,
          })}
          checked={crossfade.enabled}
          disabled={!crossfade.hydrated}
          onToggle={() => {
            onTouch();
            crossfade.toggle();
          }}
        />
        <FeatureCard
          art="automix"
          title={t("onboarding.listening.automix.title")}
          benefit={t("onboarding.listening.automix.benefit")}
          checked={autoMix.enabled}
          disabled={!autoMix.hydrated || !crossfade.hydrated}
          onToggle={() => {
            onTouch();
            // AutoMix places the crossfade; with no crossfade length set
            // it has nothing to place, so turning it on turns that on.
            if (!autoMix.enabled && !crossfade.enabled) crossfade.toggle();
            autoMix.toggle();
          }}
        />
        <FeatureCard
          art="replaygain"
          title={t("onboarding.listening.replaygain.title")}
          benefit={t("onboarding.listening.replaygain.benefit")}
          checked={replayGain.enabled}
          disabled={!replayGain.hydrated}
          onToggle={() => {
            onTouch();
            replayGain.toggle();
          }}
        />
        <FeatureCard
          art="analysis"
          title={t("onboarding.listening.analysis.title")}
          benefit={t("onboarding.listening.analysis.benefit")}
          checked={analysis.enabled}
          disabled={!analysis.hydrated}
          onToggle={() => {
            onTouch();
            analysis.toggle();
          }}
        />
      </div>
    </div>
  );
}

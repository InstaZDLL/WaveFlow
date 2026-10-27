import { useTranslation } from "react-i18next";
import {
  DSD_PRECISION_TAPS,
  REPLAYGAIN_ADJUST_LIMIT_DB,
  REPLAYGAIN_MODES,
  type ReplayGainMode,
} from "../../../../lib/tauri/player";
import { EqualizerCard } from "../EqualizerCard";
import { ExclusiveModeCard } from "../ExclusiveModeCard";
import { SettingsCategoryHeader } from "../SettingsCategoryHeader";
import { SettingsChoiceCards } from "../SettingsChoiceCards";
import { SettingsGroup } from "../SettingsGroup";
import {
  CrossfadePreview,
  PrecisionBars,
  ReplayGainPreview,
} from "../settingsPreviews";
import { StereoImageCard } from "../StereoImageCard";
import { TestToneCard } from "../TestToneCard";
import { ToggleSwitch } from "../ToggleSwitch";
import {
  AudioWaveform,
  ChevronsRight,
  Gauge,
  Headphones,
  Shuffle,
  Sparkles,
  Unplug,
  Volume2,
} from "lucide-react";

interface AudioSectionProps {
  crossfadeSec: number;
  dopPlatformSupported: boolean;
  dsdDop: boolean;
  dsdTaps: 256 | 1024 | 2048;
  dynamicCrossfade: boolean;
  dynamicCrossfadeHydrated: boolean;
  gapless: boolean;
  handleCrossfadeChange: (sec: number) => void;
  handleReplayGainFallbackChange: (value: number) => void;
  handleReplayGainModeChange: (next: ReplayGainMode) => void;
  handleReplayGainPreampChange: (value: number) => void;
  handleSetDsdPrecision: (next: 256 | 1024 | 2048) => void;
  handleToggleDsdDop: () => void;
  handleToggleDynamicCrossfade: () => void;
  handleToggleGapless: () => void;
  handleToggleMatchSourceRate: () => void;
  handleToggleMono: () => void;
  handleToggleNormalize: () => void;
  handleTogglePauseOnDeviceLoss: () => void;
  handleToggleReplayGain: () => void;
  handleToggleReplayGainPreventClipping: () => void;
  handleToggleSmartCrossfade: () => void;
  matchSourceRate: boolean;
  mono: boolean;
  normalize: boolean;
  pauseOnDeviceLoss: boolean;
  replayGain: boolean;
  replayGainFallback: number;
  replayGainMode: ReplayGainMode;
  replayGainPreamp: number;
  replayGainPreventClipping: boolean;
  smartCrossfade: boolean;
  smartCrossfadeHydrated: boolean;
  targetGroup: string | null;
}

export function AudioSection({
  crossfadeSec,
  dopPlatformSupported,
  dsdDop,
  dsdTaps,
  dynamicCrossfade,
  dynamicCrossfadeHydrated,
  gapless,
  handleCrossfadeChange,
  handleReplayGainFallbackChange,
  handleReplayGainModeChange,
  handleReplayGainPreampChange,
  handleSetDsdPrecision,
  handleToggleDsdDop,
  handleToggleDynamicCrossfade,
  handleToggleGapless,
  handleToggleMatchSourceRate,
  handleToggleMono,
  handleToggleNormalize,
  handleTogglePauseOnDeviceLoss,
  handleToggleReplayGain,
  handleToggleReplayGainPreventClipping,
  handleToggleSmartCrossfade,
  matchSourceRate,
  mono,
  normalize,
  pauseOnDeviceLoss,
  replayGain,
  replayGainFallback,
  replayGainMode,
  replayGainPreamp,
  replayGainPreventClipping,
  smartCrossfade,
  smartCrossfadeHydrated,
  targetGroup,
}: AudioSectionProps) {
  const { t } = useTranslation();
  return (
    <section
      aria-labelledby="settings-heading-playback"
      className="settings-category"
    >
      <SettingsCategoryHeader category="playback" />
      <SettingsGroup id="output" reveal={targetGroup === "output"}>
        <ExclusiveModeCard />
        {dopPlatformSupported && (
          <div className="settings-row flex items-center justify-between">
            <div className="flex items-center space-x-4 min-w-0">
              <AudioWaveform
                size={16}
                className="settings-row-icon"
                aria-hidden="true"
              />
              <div className="min-w-0">
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.dsdDop.title")}
                </div>
                <div className="text-xs settings-description">
                  {t("settings.dsdDop.subtitle")}
                </div>
              </div>
            </div>
            <ToggleSwitch
              enabled={dsdDop}
              onToggle={handleToggleDsdDop}
              label={t("settings.dsdDop.title")}
            />
          </div>
        )}
        <div className="settings-block">
          <div className="flex items-center space-x-4 min-w-0">
            <AudioWaveform
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div className="min-w-0">
              <div
                id="settings-dsd-precision-label"
                className="text-sm font-medium text-zinc-900 dark:text-white"
              >
                {t("settings.dsdPrecision.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.dsdPrecision.subtitle")}
              </div>
            </div>
          </div>
          <div className="mt-4">
            <SettingsChoiceCards
              compact
              minWidth={8}
              labelledBy="settings-dsd-precision-label"
              value={dsdTaps}
              onChange={(taps) => handleSetDsdPrecision(taps)}
              choices={DSD_PRECISION_TAPS.map((taps, index) => ({
                value: taps,
                label:
                  taps === 256
                    ? t("settings.dsdPrecision.standard")
                    : taps === 1024
                      ? t("settings.dsdPrecision.high")
                      : t("settings.dsdPrecision.max"),
                // "Taps: 1024" rather than "1024 taps": no plural to agree.
                hint: t("settings.dsdPrecision.tapsValue", {
                  value: taps,
                }),
                preview: (
                  <PrecisionBars
                    level={index + 2}
                    of={DSD_PRECISION_TAPS.length + 1}
                  />
                ),
              }))}
            />
          </div>
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Unplug
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.pauseOnDeviceLoss.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.pauseOnDeviceLoss.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={pauseOnDeviceLoss}
            onToggle={handleTogglePauseOnDeviceLoss}
            label={t("settings.pauseOnDeviceLoss.title")}
          />
        </div>
        <TestToneCard />
      </SettingsGroup>
      <SettingsGroup id="transitions" reveal={targetGroup === "transitions"}>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <ChevronsRight
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.gapless.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.gapless.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={gapless}
            onToggle={handleToggleGapless}
            label={t("settings.gapless.title")}
          />
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Shuffle
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.crossfade.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.crossfade.subtitle")}
              </div>
            </div>
          </div>
          <div className="flex items-center space-x-3">
            <CrossfadePreview seconds={crossfadeSec} max={12} />
            <input
              type="range"
              min={0}
              max={12}
              step={1}
              value={crossfadeSec}
              onChange={(e) => handleCrossfadeChange(Number(e.target.value))}
              className="w-32 h-1.5 rounded-full appearance-none bg-zinc-200 dark:bg-zinc-700 accent-emerald-500 cursor-pointer"
              aria-label={t("settings.crossfade.title")}
            />
            <span className="text-sm font-medium text-zinc-500 w-10 text-right tabular-nums">
              {crossfadeSec} s
            </span>
          </div>
        </div>
        <div className="settings-dependent">
          <div className="settings-row flex items-center justify-between">
            <div className="flex items-center space-x-4">
              <Sparkles
                size={16}
                className="settings-row-icon"
                aria-hidden="true"
              />
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.smartCrossfade.title")}
                </div>
                <div className="text-xs settings-description">
                  {t("settings.smartCrossfade.subtitle")}
                </div>
              </div>
            </div>
            <ToggleSwitch
              enabled={smartCrossfade}
              onToggle={handleToggleSmartCrossfade}
              disabled={!smartCrossfadeHydrated}
              label={t("settings.smartCrossfade.title")}
            />
          </div>
          <div className="settings-row flex items-center justify-between">
            <div className="flex items-center space-x-4">
              <Gauge
                size={16}
                className="settings-row-icon"
                aria-hidden="true"
              />
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.dynamicCrossfade.title")}
                </div>
                <div className="text-xs settings-description">
                  {t("settings.dynamicCrossfade.subtitle")}
                </div>
              </div>
            </div>
            <ToggleSwitch
              enabled={dynamicCrossfade}
              onToggle={handleToggleDynamicCrossfade}
              disabled={!dynamicCrossfadeHydrated}
              label={t("settings.dynamicCrossfade.title")}
            />
          </div>
        </div>
      </SettingsGroup>
      <SettingsGroup id="volume" reveal={targetGroup === "volume"}>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Volume2
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.normalize.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.normalize.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={normalize}
            onToggle={handleToggleNormalize}
            label={t("settings.normalize.title")}
          />
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Volume2
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.replayGain.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.replayGain.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={replayGain}
            onToggle={handleToggleReplayGain}
            label={t("settings.replayGain.title")}
          />
        </div>
        {replayGain && (
          <div className="settings-dependent">
            {/* Pre-amp */}
            <div className="settings-row flex items-center justify-between">
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.replayGain.preamp.title")}
                </div>
                <div className="text-xs settings-description">
                  {t("settings.replayGain.preamp.subtitle")}
                </div>
              </div>
              <div className="flex items-center space-x-3">
                <input
                  type="range"
                  min={-REPLAYGAIN_ADJUST_LIMIT_DB}
                  max={REPLAYGAIN_ADJUST_LIMIT_DB}
                  step={0.5}
                  value={replayGainPreamp}
                  onChange={(e) =>
                    handleReplayGainPreampChange(Number(e.target.value))
                  }
                  className="w-32 h-1.5 rounded-full appearance-none bg-zinc-200 dark:bg-zinc-700 accent-emerald-500 cursor-pointer"
                  aria-label={t("settings.replayGain.preamp.title")}
                />
                <span className="text-sm font-medium text-zinc-500 w-16 text-right tabular-nums">
                  {t("settings.replayGain.decibels", {
                    value: replayGainPreamp.toFixed(1),
                  })}
                </span>
              </div>
            </div>

            {/* Fallback gain for untagged, unanalysed tracks */}
            <div className="settings-row flex items-center justify-between">
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.replayGain.fallback.title")}
                </div>
                <div className="text-xs settings-description">
                  {t("settings.replayGain.fallback.subtitle")}
                </div>
              </div>
              <div className="flex items-center space-x-3">
                <input
                  type="range"
                  min={-REPLAYGAIN_ADJUST_LIMIT_DB}
                  max={REPLAYGAIN_ADJUST_LIMIT_DB}
                  step={0.5}
                  value={replayGainFallback}
                  onChange={(e) =>
                    handleReplayGainFallbackChange(Number(e.target.value))
                  }
                  className="w-32 h-1.5 rounded-full appearance-none bg-zinc-200 dark:bg-zinc-700 accent-emerald-500 cursor-pointer"
                  aria-label={t("settings.replayGain.fallback.title")}
                />
                <span className="text-sm font-medium text-zinc-500 w-16 text-right tabular-nums">
                  {t("settings.replayGain.decibels", {
                    value: replayGainFallback.toFixed(1),
                  })}
                </span>
              </div>
            </div>

            {/* Track gain vs album gain (#587) */}
            <div className="settings-block">
              <div
                id="settings-replaygain-mode-label"
                className="text-sm font-medium text-zinc-900 dark:text-white"
              >
                {t("settings.replayGain.mode.title")}
              </div>
              <div className="mt-3">
                <SettingsChoiceCards
                  compact
                  minWidth={9}
                  labelledBy="settings-replaygain-mode-label"
                  value={replayGainMode}
                  onChange={(mode) => handleReplayGainModeChange(mode)}
                  choices={REPLAYGAIN_MODES.map((mode) => ({
                    value: mode,
                    label: t(`settings.replayGain.mode.${mode}`),
                    hint: t(`settings.replayGain.mode.${mode}Hint`),
                    preview: <ReplayGainPreview mode={mode} />,
                  }))}
                />
              </div>
            </div>

            {/* Clipping prevention */}
            <div className="settings-row flex items-center justify-between">
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.replayGain.preventClipping.title")}
                </div>
                <div className="text-xs settings-description">
                  {t("settings.replayGain.preventClipping.subtitle")}
                </div>
              </div>
              <ToggleSwitch
                enabled={replayGainPreventClipping}
                onToggle={handleToggleReplayGainPreventClipping}
                label={t("settings.replayGain.preventClipping.title")}
              />
            </div>
          </div>
        )}
      </SettingsGroup>
      <SettingsGroup id="equalizer" bare reveal={targetGroup === "equalizer"}>
        <EqualizerCard />
      </SettingsGroup>
      <SettingsGroup id="stereo" reveal={targetGroup === "stereo"}>
        <StereoImageCard />
      </SettingsGroup>
      <SettingsGroup
        id="outputAdvanced"
        advanced
        reveal={targetGroup === "outputAdvanced"}
      >
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Gauge size={16} className="settings-row-icon" aria-hidden="true" />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.matchSourceRate.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.matchSourceRate.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={matchSourceRate}
            onToggle={handleToggleMatchSourceRate}
            label={t("settings.matchSourceRate.title")}
          />
        </div>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Headphones
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.mono.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.mono.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={mono}
            onToggle={handleToggleMono}
            label={t("settings.mono.title")}
          />
        </div>
      </SettingsGroup>
    </section>
  );
}

import { useTranslation } from "react-i18next";
import {
  UI_ZOOM_MAX,
  UI_ZOOM_MIN,
  UI_ZOOM_STEP,
} from "../../../../lib/tauri/preferences";
import { AccentCard } from "../AccentCard";
import { ArtistHeroCard } from "../ArtistHeroCard";
import { ContrastCard } from "../ContrastCard";
import { CoverSlideshowCard } from "../CoverSlideshowCard";
import { EffectsCard } from "../EffectsCard";
import { HiResBadgeCard } from "../HiResBadgeCard";
import { ImmersiveViewCard } from "../ImmersiveViewCard";
import { MiniPlayerBoundsCard } from "../MiniPlayerBoundsCard";
import { MotionCoversCard } from "../MotionCoversCard";
import { PlayerBarLayoutCard } from "../PlayerBarLayoutCard";
import { ScrollTitlesCard } from "../ScrollTitlesCard";
import { SettingsCategoryHeader } from "../SettingsCategoryHeader";
import { SettingsGroup } from "../SettingsGroup";
import { SkinPickerCard } from "../SkinPickerCard";
import { StatsKpiVisibilityCard } from "../StatsKpiVisibilityCard";
import { ThemePickerCard } from "../ThemePickerCard";
import { ToggleSwitch } from "../ToggleSwitch";
import { WindowBoundsCard } from "../WindowBoundsCard";
import { WindowChromeCard } from "../WindowChromeCard";
import { WrappedBannerCard } from "../WrappedBannerCard";
import { XwaylandCard } from "../XwaylandCard";
import { Activity, ZoomIn } from "lucide-react";

interface AppearanceSectionProps {
  handleToggleVisualizer: () => void;
  handleZoomDelta: (delta: number) => void;
  handleZoomReset: () => void;
  targetGroup: string | null;
  uiZoom: number;
  visualizer: boolean;
  visualizerHydrated: boolean;
}

export function AppearanceSection({
  handleToggleVisualizer,
  handleZoomDelta,
  handleZoomReset,
  targetGroup,
  uiZoom,
  visualizer,
  visualizerHydrated,
}: AppearanceSectionProps) {
  const { t } = useTranslation();
  return (
    <section
      aria-labelledby="settings-heading-appearance"
      className="settings-category"
    >
      <SettingsCategoryHeader category="appearance" />
      <SettingsGroup id="skin" reveal={targetGroup === "skin"}>
        <SkinPickerCard />
      </SettingsGroup>
      <SettingsGroup id="theme" reveal={targetGroup === "theme"}>
        <ThemePickerCard />
        <AccentCard />
      </SettingsGroup>
      <SettingsGroup id="display" reveal={targetGroup === "display"}>
        <ContrastCard />
        <EffectsCard />
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <ZoomIn
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.uiZoom.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.uiZoom.subtitle")}
              </div>
            </div>
          </div>
          <div className="flex items-center space-x-2">
            <button
              type="button"
              onClick={() => handleZoomDelta(-UI_ZOOM_STEP)}
              disabled={uiZoom <= UI_ZOOM_MIN + 1e-3}
              aria-label={t("settings.uiZoom.decreaseAria")}
              className="w-8 h-8 flex items-center justify-center rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700 disabled:opacity-40 disabled:cursor-not-allowed text-lg leading-none"
            >
              −
            </button>
            <button
              type="button"
              onClick={handleZoomReset}
              aria-label={t("settings.uiZoom.resetAria")}
              title={t("settings.uiZoom.resetAria")}
              className="min-w-14 px-2 h-8 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700 text-sm font-mono tabular-nums"
            >
              {Math.round(uiZoom * 100)} %
            </button>
            <button
              type="button"
              onClick={() => handleZoomDelta(UI_ZOOM_STEP)}
              disabled={uiZoom >= UI_ZOOM_MAX - 1e-3}
              aria-label={t("settings.uiZoom.increaseAria")}
              className="w-8 h-8 flex items-center justify-center rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700 disabled:opacity-40 disabled:cursor-not-allowed text-lg leading-none"
            >
              +
            </button>
          </div>
        </div>
      </SettingsGroup>
      <SettingsGroup id="player" reveal={targetGroup === "player"}>
        <PlayerBarLayoutCard />
        <HiResBadgeCard />
        <ScrollTitlesCard />
      </SettingsGroup>
      <SettingsGroup id="window" reveal={targetGroup === "window"}>
        <WindowBoundsCard />
        <MiniPlayerBoundsCard />
        <WindowChromeCard />
        <XwaylandCard />
      </SettingsGroup>
      <SettingsGroup id="immersive" reveal={targetGroup === "immersive"}>
        <ImmersiveViewCard />
        <CoverSlideshowCard />
        <MotionCoversCard />
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Activity
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.visualizer.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.visualizer.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={visualizer}
            onToggle={handleToggleVisualizer}
            disabled={!visualizerHydrated}
            label={t("settings.visualizer.title")}
          />
        </div>
      </SettingsGroup>
      <SettingsGroup id="pages" reveal={targetGroup === "pages"}>
        <ArtistHeroCard />
        <StatsKpiVisibilityCard />
        <WrappedBannerCard />
      </SettingsGroup>
    </section>
  );
}

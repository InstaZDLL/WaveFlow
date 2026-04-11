import { useTranslation } from "react-i18next";
import { Menu, MonitorSpeaker } from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";
import { Artwork } from "../common/Artwork";
import { PlaybackControls } from "./PlaybackControls";
import { ProgressBar } from "./ProgressBar";
import { VolumeControl } from "./VolumeControl";

export function PlayerBar() {
  const { t } = useTranslation();
  const {
    isQueueOpen,
    toggleQueue,
    isDeviceMenuOpen,
    toggleDeviceMenu,
    currentTrack,
  } = usePlayer();

  const title = currentTrack?.title ?? t("player.noTrack");
  const subtitle =
    currentTrack?.artist_name ??
    currentTrack?.album_title ??
    t("player.inactive");

  return (
    <div className="h-24 px-6 flex items-center justify-between border-t z-50 bg-[#FAFAFA] border-zinc-200 text-zinc-600 dark:bg-surface-dark-elevated dark:border-zinc-800 dark:text-zinc-300">
      {/* Left: Track Info */}
      <div className="w-1/3 flex items-center space-x-4 min-w-0">
        <Artwork
          path={currentTrack?.artwork_path ?? null}
          className="w-14 h-14 shadow-sm border border-zinc-200 dark:border-transparent"
          iconSize={24}
          alt={title}
          rounded="xl"
        />
        <div className="flex flex-col min-w-0">
          <span className="text-sm font-semibold text-zinc-900 dark:text-zinc-100 truncate">
            {title}
          </span>
          <span className="text-[11px] text-zinc-500 dark:text-zinc-400 truncate">
            {subtitle}
          </span>
        </div>
      </div>

      {/* Center: Controls */}
      <div className="w-1/3 flex flex-col items-center max-w-md">
        <PlaybackControls />
        <ProgressBar />
      </div>

      {/* Right: Extra Controls */}
      <div className="w-1/3 flex items-center justify-end space-x-4">
        <button
          onClick={toggleQueue}
          className={`p-2 rounded-lg transition-colors ${
            isQueueOpen
              ? "text-emerald-500"
              : "text-zinc-400 hover:text-zinc-800 dark:hover:text-white"
          }`}
        >
          <Menu size={20} />
        </button>

        <div className="relative">
          <button
            onClick={toggleDeviceMenu}
            className={`p-2 rounded-lg transition-colors border ${
              isDeviceMenuOpen
                ? "border-emerald-500 text-emerald-500 bg-emerald-500/10"
                : "border-transparent text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-800"
            }`}
          >
            <MonitorSpeaker size={20} />
          </button>
        </div>

        <VolumeControl />
      </div>
    </div>
  );
}

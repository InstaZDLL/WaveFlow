import { useTranslation } from "react-i18next";
import { Music2, Menu, MonitorSpeaker } from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";
import { PlaybackControls } from "./PlaybackControls";
import { ProgressBar } from "./ProgressBar";
import { VolumeControl } from "./VolumeControl";

export function PlayerBar() {
  const { t } = useTranslation();
  const { isQueueOpen, toggleQueue, isDeviceMenuOpen, toggleDeviceMenu } =
    usePlayer();

  return (
    <div className="h-24 px-6 flex items-center justify-between border-t z-50 bg-[#FAFAFA] border-zinc-200 text-zinc-600 dark:bg-surface-dark-elevated dark:border-zinc-800 dark:text-zinc-300">
      {/* Left: Track Info */}
      <div className="w-1/3 flex items-center space-x-4">
        <div className="w-14 h-14 rounded-xl flex items-center justify-center shadow-sm bg-white border border-zinc-200 dark:bg-zinc-800 dark:border-transparent">
          <Music2 size={24} className="text-zinc-300" />
        </div>
        <div className="flex flex-col">
          <span className="text-sm font-semibold text-zinc-500 dark:text-zinc-400">
            {t("player.noTrack")}
          </span>
          <span className="text-[10px] text-zinc-400 mt-0.5">
            {t("player.inactive")}
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

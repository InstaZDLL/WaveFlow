import { createContext, useContext } from "react";

export type RepeatMode = "off" | "all" | "one";

interface PlayerContextValue {
  isPlaying: boolean;
  togglePlayback: () => void;
  isQueueOpen: boolean;
  toggleQueue: () => void;
  isDeviceMenuOpen: boolean;
  toggleDeviceMenu: () => void;
  volume: number;
  setVolume: (value: number) => void;
  toggleMute: () => void;
  isShuffled: boolean;
  toggleShuffle: () => void;
  repeatMode: RepeatMode;
  cycleRepeatMode: () => void;
}

export const PlayerContext = createContext<PlayerContextValue | null>(null);

export function usePlayer() {
  const context = useContext(PlayerContext);
  if (!context)
    throw new Error("usePlayer must be used within PlayerProvider");
  return context;
}

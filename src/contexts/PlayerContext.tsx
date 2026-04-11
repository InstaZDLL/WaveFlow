import { useCallback, useRef, useState, type ReactNode } from "react";
import { PlayerContext, type RepeatMode } from "../hooks/usePlayer";

export function PlayerProvider({ children }: { children: ReactNode }) {
  const [isPlaying, setIsPlaying] = useState(false);
  const [isQueueOpen, setIsQueueOpen] = useState(false);
  const [isDeviceMenuOpen, setIsDeviceMenuOpen] = useState(false);
  const [volume, setVolumeState] = useState(80);
  const [isShuffled, setIsShuffled] = useState(false);
  const [repeatMode, setRepeatMode] = useState<RepeatMode>("off");
  const previousVolumeRef = useRef(80);

  const togglePlayback = () => setIsPlaying((prev) => !prev);
  const toggleQueue = () => setIsQueueOpen((prev) => !prev);
  const toggleDeviceMenu = () => setIsDeviceMenuOpen((prev) => !prev);
  const toggleShuffle = () => setIsShuffled((prev) => !prev);

  const cycleRepeatMode = useCallback(() => {
    setRepeatMode((prev) => {
      if (prev === "off") return "all";
      if (prev === "all") return "one";
      return "off";
    });
  }, []);

  const setVolume = useCallback((value: number) => {
    const clamped = Math.max(0, Math.min(100, Math.round(value)));
    setVolumeState(clamped);
    if (clamped > 0) previousVolumeRef.current = clamped;
  }, []);

  const toggleMute = useCallback(() => {
    setVolumeState((current) => {
      if (current > 0) {
        previousVolumeRef.current = current;
        return 0;
      }
      return previousVolumeRef.current || 50;
    });
  }, []);

  return (
    <PlayerContext.Provider
      value={{
        isPlaying,
        togglePlayback,
        isQueueOpen,
        toggleQueue,
        isDeviceMenuOpen,
        toggleDeviceMenu,
        volume,
        setVolume,
        toggleMute,
        isShuffled,
        toggleShuffle,
        repeatMode,
        cycleRepeatMode,
      }}
    >
      {children}
    </PlayerContext.Provider>
  );
}

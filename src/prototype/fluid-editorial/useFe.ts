import { createContext, useContext } from "react";
import type { copyFor, Lang } from "./copy";
import type { Atmosphere } from "./color";
import type { FeTrack, MoodId } from "./fixtures";

export interface FeState {
  lang: Lang;
  copy: ReturnType<typeof copyFor>;
  current: FeTrack;
  queue: FeTrack[];
  isPlaying: boolean;
  positionMs: number;
  /** Set while a mood radio is "playing": its id, for the labels. */
  radioMood: MoodId | null;
  atmosphere: Atmosphere;
  liked: boolean;
  play: (track: FeTrack, opts?: { radio?: MoodId | null }) => void;
  togglePlay: () => void;
  seek: (ms: number) => void;
  next: () => void;
  previous: () => void;
  toggleLike: () => void;
}

export const FeContext = createContext<FeState | null>(null);

export function useFe(): FeState {
  const v = useContext(FeContext);
  if (!v) throw new Error("useFe outside FeStateProvider");
  return v;
}

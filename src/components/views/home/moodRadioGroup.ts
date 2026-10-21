import { useMemo, useRef, useState, type KeyboardEvent } from "react";
import type { Mood, MoodCounts } from "../../../lib/tauri/moodRadio";

export interface MoodDef {
  id: Mood;
  /** Tempo window, in BPM — the same gates `mood_radio` applies. */
  bpm: [number, number];
  /**
   * Cycles of the abstract wave across one segment width. A whole
   * number: the drift translates the drawing by one segment width, and
   * only a whole number of periods lands on itself.
   */
  cycles: number;
  /** 0–1, how tall the wave is. */
  amplitude: number;
  /** Seconds for one drift of the wave; slower moods drift slower. */
  period: number;
}

/** The five moods, ordered by tempo, which is what left-to-right means. */
export const MOODS: MoodDef[] = [
  { id: "sleep", bpm: [40, 80], cycles: 1, amplitude: 0.22, period: 16 },
  { id: "focus", bpm: [60, 100], cycles: 2, amplitude: 0.36, period: 12 },
  { id: "chill", bpm: [70, 110], cycles: 3, amplitude: 0.48, period: 10 },
  { id: "party", bpm: [110, 135], cycles: 5, amplitude: 0.72, period: 7 },
  { id: "workout", bpm: [120, 170], cycles: 7, amplitude: 0.9, period: 5 },
];

/**
 * A seamless sine over two segment widths (400 units), so translating
 * the drawing by −50 % loops; `height` is the viewBox height the
 * caller draws it in.
 */
export function wavePath(
  cycles: number,
  amplitude: number,
  height: number,
): string {
  const W = 400;
  const mid = height / 2;
  const amp = (height / 2 - 4) * amplitude;
  const steps = 96;
  const pts: string[] = [];
  for (let i = 0; i <= steps; i++) {
    const x = (i / steps) * W;
    const y = mid + Math.sin((x / (W / 2)) * cycles * Math.PI * 2) * amp;
    pts.push(`${i === 0 ? "M" : "L"}${x.toFixed(1)} ${y.toFixed(2)}`);
  }
  return pts.join(" ");
}

/**
 * The radiogroup behind both drawings of Mood Radio on the fluid layout
 * (the spectrum, the capsules): which mood is current, roving focus over
 * the moods that have tracks (←/→, Home/End), Enter or Space starting
 * the radio. The components keep only their markup.
 */
export function useMoodRadioGroup(
  counts: MoodCounts | null,
  onStart: (mood: Mood) => void,
  waveHeight: number,
) {
  const enabled = MOODS.filter((m) => (counts?.[m.id] ?? 0) > 0);
  const [selected, setSelected] = useState<Mood | null>(null);
  const refs = useRef<Record<string, HTMLButtonElement | null>>({});
  const paths = useMemo(
    () =>
      Object.fromEntries(
        MOODS.map((m) => [m.id, wavePath(m.cycles, m.amplitude, waveHeight)]),
      ),
    [waveHeight],
  );
  // Nothing selected until the counts arrive; then Focus if it has
  // tracks, else the first mood that does.
  const current: Mood | null =
    selected && (counts?.[selected] ?? 0) > 0
      ? selected
      : (counts?.focus ?? 0) > 0
        ? "focus"
        : (enabled[0]?.id ?? null);

  const onKeyDown = (e: KeyboardEvent<HTMLButtonElement>, mood: MoodDef) => {
    const index = enabled.indexOf(mood);
    let to: number | null = null;
    if (e.key === "ArrowRight") to = (index + 1) % enabled.length;
    else if (e.key === "ArrowLeft")
      to = (index - 1 + enabled.length) % enabled.length;
    else if (e.key === "Home") to = 0;
    else if (e.key === "End") to = enabled.length - 1;
    else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onStart(mood.id);
      return;
    }
    if (to == null || enabled.length === 0) return;
    e.preventDefault();
    const m = enabled[to];
    setSelected(m.id);
    refs.current[m.id]?.focus();
  };

  return { current, setSelected, refs, paths, onKeyDown };
}

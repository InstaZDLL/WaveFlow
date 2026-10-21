import { useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import { Play } from "lucide-react";
import type { Mood, MoodCounts } from "../../../lib/tauri/moodRadio";

interface MoodDef {
  id: Mood;
  /** Tempo window, in BPM — the same gates `mood_radio` applies. */
  bpm: [number, number];
  /** Cycles of the inner light wave across one capsule width. */
  cycles: number;
  /** 0–1, how tall the wave is. */
  amplitude: number;
  /** Seconds for one drift of the wave; slower moods drift slower. */
  period: number;
}

/** Ordered by tempo, left to right. */
const MOODS: MoodDef[] = [
  { id: "sleep", bpm: [40, 80], cycles: 1.5, amplitude: 0.3, period: 16 },
  { id: "focus", bpm: [60, 100], cycles: 2.5, amplitude: 0.42, period: 12 },
  { id: "chill", bpm: [70, 110], cycles: 3, amplitude: 0.52, period: 10 },
  { id: "party", bpm: [110, 135], cycles: 5, amplitude: 0.72, period: 7 },
  { id: "workout", bpm: [120, 170], cycles: 7, amplitude: 0.9, period: 5 },
];

/** A seamless sine over two capsule widths, so translating by −50 % loops. */
function wavePath(cycles: number, amplitude: number): string {
  const W = 400;
  const H = 40;
  const mid = H / 2;
  const amp = (H / 2 - 3) * amplitude;
  const steps = 96;
  const pts: string[] = [];
  for (let i = 0; i <= steps; i++) {
    const x = (i / steps) * W;
    const y = mid + Math.sin((x / (W / 2)) * cycles * Math.PI * 2) * amp;
    pts.push(`${i === 0 ? "M" : "L"}${x.toFixed(1)} ${y.toFixed(2)}`);
  }
  return pts.join(" ");
}

interface MoodCapsulesProps {
  counts: MoodCounts | null;
  /** The mood whose radio is being built, if any. */
  loadingMood: Mood | null;
  onStart: (mood: Mood) => void;
}

/**
 * Mood Radio as five capsules of tinted glass — the Liquid skin's
 * drawing of it, where Fluid draws one tempo band. Each mood is its own
 * object of light: a sheet in its pastel, a slow inner wave, the name,
 * the tempo window and how many tracks qualify. Choosing one brightens
 * it and reveals its subtitle and the start pill. The five stay named
 * and clickable; nothing depends on reading a colour.
 *
 * Keyboard: a radiogroup with roving focus over the moods that have
 * tracks (←/→, Home/End); Enter or Space on the focused capsule starts
 * the radio, as does the pill beside it.
 */
export function MoodCapsules({
  counts,
  loadingMood,
  onStart,
}: MoodCapsulesProps) {
  const { t } = useTranslation();
  const enabled = MOODS.filter((m) => (counts?.[m.id] ?? 0) > 0);
  const [selected, setSelected] = useState<Mood | null>(null);
  const refs = useRef<Record<string, HTMLButtonElement | null>>({});
  const paths = useMemo(
    () =>
      Object.fromEntries(
        MOODS.map((m) => [m.id, wavePath(m.cycles, m.amplitude)]),
      ),
    [],
  );
  const current =
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

  return (
    <div
      className="aq-capsules"
      role="radiogroup"
      aria-label={t("home.moodRadio.title")}
    >
      {MOODS.map((m) => {
        const count = counts?.[m.id] ?? 0;
        const disabled = counts != null && count === 0;
        const isSel = current === m.id;
        const title = t(`home.moodRadio.${m.id}.title`);
        const subtitle = t(`home.moodRadio.${m.id}.subtitle`);
        const ctaLabel =
          loadingMood === m.id
            ? t("home.moodRadio.loading")
            : t("home.fluid.moodStart");
        const countLabel = disabled
          ? t("home.moodRadio.empty")
          : t("home.moodRadio.trackCount", { count });
        return (
          // The capsule is a wrapper: the radio fills it, and the start
          // pill is its sibling, not its child — a control inside a
          // control is invalid HTML and reads wrong to assistive tech.
          <div
            key={m.id}
            className="aq-capsule"
            data-mood={m.id}
            data-checked={isSel}
            data-disabled={disabled || undefined}
            style={{ ["--aq-wave-period" as string]: `${m.period}s` }}
          >
            <button
              ref={(el) => {
                refs.current[m.id] = el;
              }}
              type="button"
              role="radio"
              aria-checked={isSel}
              aria-label={`${title}, ${m.bpm[0]}–${m.bpm[1]} BPM, ${countLabel}`}
              tabIndex={isSel ? 0 : -1}
              disabled={disabled}
              onClick={() => setSelected(m.id)}
              onKeyDown={(e) => onKeyDown(e, m)}
              className="aq-capsule__radio"
            >
              <span className="aq-capsule__light" aria-hidden="true">
                <svg viewBox="0 0 400 40" preserveAspectRatio="none">
                  <path
                    d={paths[m.id]}
                    fill="none"
                    stroke="currentColor"
                    strokeWidth={isSel ? 1.8 : 1.3}
                    vectorEffect="non-scaling-stroke"
                  />
                </svg>
              </span>
              <span className="aq-capsule__head">
                <span className="fe-title aq-capsule__name">{title}</span>
                <span className="fe-mono aq-capsule__bpm">
                  {m.bpm[0]}–{m.bpm[1]}
                </span>
              </span>
              <span className="fe-mono aq-capsule__count">{countLabel}</span>
              <span className="aq-capsule__detail">
                <span className="aq-capsule__sub">{subtitle}</span>
              </span>
            </button>
            {isSel && !disabled && (
              <button
                type="button"
                onClick={() => onStart(m.id)}
                disabled={loadingMood != null}
                aria-label={`${t("home.fluid.moodStart")} — ${title}`}
                className="aq-capsule__cta fe-press"
              >
                <Play size={12} className="fill-current" strokeWidth={0} />
                {ctaLabel}
              </button>
            )}
          </div>
        );
      })}
    </div>
  );
}

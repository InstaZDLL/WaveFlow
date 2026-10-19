import { useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import { Play } from "lucide-react";
import type { Mood, MoodCounts } from "../../../lib/tauri/moodRadio";

interface MoodDef {
  id: Mood;
  /** Tempo window, in BPM — the same gates `mood_radio` applies. */
  bpm: [number, number];
  /** Cycles of the abstract wave across one segment width. */
  cycles: number;
  /** 0–1, how tall the wave is. */
  amplitude: number;
  /** Seconds for one drift of the wave; slower moods drift slower. */
  period: number;
}

/** Ordered by tempo, which is what the horizontal axis means. */
const MOODS: MoodDef[] = [
  { id: "sleep", bpm: [40, 80], cycles: 1.5, amplitude: 0.22, period: 16 },
  { id: "focus", bpm: [60, 100], cycles: 2.5, amplitude: 0.36, period: 12 },
  { id: "chill", bpm: [70, 110], cycles: 3, amplitude: 0.48, period: 10 },
  { id: "party", bpm: [110, 135], cycles: 5, amplitude: 0.72, period: 7 },
  { id: "workout", bpm: [120, 170], cycles: 7, amplitude: 0.9, period: 5 },
];

/** A seamless sine over two segment widths, so translating by −50 % loops. */
function wavePath(cycles: number, amplitude: number): string {
  const W = 400;
  const H = 60;
  const mid = H / 2;
  const amp = (H / 2 - 4) * amplitude;
  const steps = 96;
  const pts: string[] = [];
  for (let i = 0; i <= steps; i++) {
    const x = (i / steps) * W;
    const y = mid + Math.sin((x / (W / 2)) * cycles * Math.PI * 2) * amp;
    pts.push(`${i === 0 ? "M" : "L"}${x.toFixed(1)} ${y.toFixed(2)}`);
  }
  return pts.join(" ");
}

interface MoodSpectrumProps {
  counts: MoodCounts | null;
  /** The mood whose radio is being built, if any. */
  loadingMood: Mood | null;
  onStart: (mood: Mood) => void;
}

/**
 * Mood Radio as a spectrum, the Fluid skin's signature: one continuous
 * band whose horizontal axis is tempo. Each mood is a segment on that
 * axis with its own abstract waveform (slower, flatter on the left;
 * faster, taller on the right). Selecting a mood widens its segment and
 * reveals what the radio is made of — tempo window, how many tracks
 * qualify — plus the one button that starts it. The five moods stay
 * named and clickable; nothing depends on reading the gradient.
 *
 * Keyboard: a radiogroup with roving focus over the moods that have
 * tracks (←/→, Home/End); Enter or Space on the focused segment starts
 * the radio, as does the start button beside it.
 */
export function MoodSpectrum({
  counts,
  loadingMood,
  onStart,
}: MoodSpectrumProps) {
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
  // Nothing selected until the counts arrive; then Focus if it has
  // tracks, else the first mood that does.
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
    <div>
      <div
        className="fe-spectrum"
        role="radiogroup"
        aria-label={t("home.moodRadio.title")}
      >
        <div className="fe-spectrum__band" aria-hidden="true" />
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
          const countLabel = t("home.moodRadio.trackCount", { count });
          return (
            // The segment is a wrapper: the radio fills it, and the start
            // button is its sibling, not its child — a control inside a
            // control is invalid HTML and reads wrong to assistive tech.
            <div
              key={m.id}
              className="fe-spectrum__seg"
              data-checked={isSel}
              data-disabled={disabled || undefined}
              style={{ ["--fe-wave-period" as string]: `${m.period}s` }}
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
                className="fe-spectrum__radio"
              >
                <div className="fe-spectrum__wave" aria-hidden="true">
                  <svg viewBox="0 0 400 60" preserveAspectRatio="none">
                    <path
                      d={paths[m.id]}
                      fill="none"
                      stroke="rgb(255 255 255 / 0.85)"
                      strokeWidth={isSel ? 1.6 : 1.2}
                      vectorEffect="non-scaling-stroke"
                    />
                  </svg>
                </div>
                <div className="relative">
                  <div className="flex items-baseline justify-between gap-3">
                    <span
                      className={`fe-title truncate ${isSel ? "text-[1.25rem]" : "text-[1rem]"}`}
                    >
                      {title}
                    </span>
                    <span className="fe-mono shrink-0 text-[0.75rem] text-white/60">
                      {m.bpm[0]}–{m.bpm[1]}
                    </span>
                  </div>
                  <div className="fe-spectrum__detail">
                    <div>
                      <p className="mt-1.5 max-w-[26ch] text-[0.8125rem] leading-snug text-white/75">
                        {subtitle}
                      </p>
                      <div className="mt-3 flex items-center gap-4">
                        {/* Reserves the start button's footprint; the
                            button itself is drawn over it, below. */}
                        <span
                          className="fe-spectrum__cta invisible"
                          aria-hidden="true"
                        >
                          <Play size={12} strokeWidth={0} />
                          {ctaLabel}
                        </span>
                        <span className="fe-mono text-[0.8125rem] text-white/60">
                          {countLabel}
                        </span>
                      </div>
                    </div>
                  </div>
                  {!isSel && (
                    <p className="fe-mono mt-1 text-[0.75rem] text-white/55">
                      {disabled ? t("home.moodRadio.empty") : countLabel}
                    </p>
                  )}
                </div>
              </button>
              {isSel && !disabled && (
                <button
                  type="button"
                  onClick={() => onStart(m.id)}
                  disabled={loadingMood != null}
                  aria-label={`${t("home.fluid.moodStart")} — ${title}`}
                  className="fe-spectrum__cta fe-press absolute bottom-4 left-[1.125rem] bg-white/[0.14] hover:bg-white/[0.22] disabled:opacity-60"
                >
                  <Play size={12} className="fill-current" strokeWidth={0} />
                  {ctaLabel}
                </button>
              )}
            </div>
          );
        })}
      </div>
      <div
        className="fe-spectrum__ruler fe-ink-3 fe-mono text-[0.6875rem] tracking-[0.08em] uppercase"
        aria-hidden="true"
      >
        <span>40 BPM</span>
        <span>{t("home.fluid.tempo")}</span>
        <span>170 BPM</span>
      </div>
    </div>
  );
}

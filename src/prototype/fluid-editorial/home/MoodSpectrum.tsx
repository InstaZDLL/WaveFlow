import { useMemo, useRef, useState, type KeyboardEvent } from "react";
import { Play } from "lucide-react";
import { fixtures, type MoodId } from "../fixtures";
import { fill } from "../copy";
import { integer } from "../format";
import { useFe } from "../useFe";
import { SectionHeader } from "../ui/SectionHeader";

interface MoodDef {
  id: MoodId;
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
    // Two "cycles" spans per 400 units so the second half repeats the first.
    const y = mid + Math.sin((x / (W / 2)) * cycles * Math.PI * 2) * amp;
    pts.push(`${i === 0 ? "M" : "L"}${x.toFixed(1)} ${y.toFixed(2)}`);
  }
  return pts.join(" ");
}

/**
 * Mood Radio as a spectrum: one continuous band whose horizontal axis
 * is tempo. Each mood is a segment on that axis with its own abstract
 * waveform (slower, flatter on the left; faster, taller on the right).
 * Selecting a mood widens its segment and reveals what the radio is
 * made of — tempo window, how many tracks qualify — plus the one
 * button that starts it. The five moods stay named and clickable;
 * nothing here depends on reading the gradient.
 *
 * Keyboard: a radiogroup with roving focus over the moods that have
 * tracks (←/→, Home/End); Enter or Space on the focused segment starts
 * the radio, as does the start button beside it.
 */
export function MoodSpectrum() {
  const { copy, lang, play, radioMood } = useFe();
  const counts = fixtures.moodCounts;
  // Only a mood with tracks can be selected: the playing radio's mood
  // if it has any, else Focus, else the first one that does.
  const enabled = MOODS.filter((m) => counts[m.id] > 0);
  const [selected, setSelected] = useState<MoodId | null>(() => {
    const prefer = [radioMood, "focus" as MoodId];
    return (
      prefer.find((id) => id != null && counts[id] > 0) ??
      enabled[0]?.id ??
      null
    );
  });
  const [starting, setStarting] = useState<MoodId | null>(null);
  const refs = useRef<Record<string, HTMLButtonElement | null>>({});
  const paths = useMemo(
    () =>
      Object.fromEntries(
        MOODS.map((m) => [m.id, wavePath(m.cycles, m.amplitude)]),
      ),
    [],
  );

  const start = (mood: MoodDef) => {
    if (starting) return;
    setStarting(mood.id);
    // Stand-in for `start_mood_radio`: a track whose tempo sits in the
    // window, or the most recent one when none is analysed.
    const pick =
      fixtures.recentPlays.find(
        (t) => t.bpm != null && t.bpm >= mood.bpm[0] && t.bpm <= mood.bpm[1],
      ) ??
      fixtures.recentPlays[1] ??
      fixtures.recentPlays[0];
    window.setTimeout(() => {
      play(pick, { radio: mood.id });
      setStarting(null);
    }, 420);
  };

  // Arrow keys walk the enabled moods only, so focus never lands on a
  // segment that cannot start a radio.
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
      start(mood);
      return;
    }
    if (to == null || enabled.length === 0) return;
    e.preventDefault();
    const m = enabled[to];
    setSelected(m.id);
    refs.current[m.id]?.focus();
  };

  return (
    <section aria-labelledby="fe-mood-title">
      <SectionHeader
        title={copy.mood.title}
        lede={copy.mood.subtitle}
        className="mb-5"
      />
      <div
        className="fe-spectrum"
        role="radiogroup"
        aria-labelledby="fe-mood-title"
      >
        <span id="fe-mood-title" className="sr-only">
          {copy.mood.title}
        </span>
        <div className="fe-spectrum__band" aria-hidden="true" />
        {MOODS.map((m) => {
          const count = counts[m.id];
          const disabled = count === 0;
          const isSel = selected === m.id;
          const label = copy.mood.moods[m.id];
          const ctaLabel =
            starting === m.id ? copy.mood.starting : copy.mood.start;
          return (
            // The segment is a wrapper: the radio fills it, and the start
            // button is its *sibling*, not its child — a control inside a
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
                aria-label={`${label.title}, ${m.bpm[0]}–${m.bpm[1]} ${copy.mood.bpm}, ${fill(copy.mood.tracks, { count })}`}
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
                      {label.title}
                    </span>
                    <span className="fe-mono shrink-0 text-[0.6875rem] text-white/60">
                      {m.bpm[0]}–{m.bpm[1]}
                    </span>
                  </div>
                  <div className="fe-spectrum__detail">
                    <div>
                      <p className="mt-1.5 max-w-[26ch] text-[0.8125rem] leading-snug text-white/75">
                        {label.subtitle}
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
                        <span className="fe-mono text-[0.75rem] text-white/60">
                          {fill(copy.mood.tracks, {
                            count: integer(count, lang),
                          })}
                        </span>
                      </div>
                    </div>
                  </div>
                  {!isSel && (
                    <p className="fe-mono mt-1 text-[0.6875rem] text-white/55">
                      {disabled
                        ? copy.mood.empty
                        : fill(copy.mood.tracks, {
                            count: integer(count, lang),
                          })}
                    </p>
                  )}
                </div>
              </button>
              {isSel && !disabled && (
                <button
                  type="button"
                  onClick={() => start(m)}
                  aria-label={`${copy.mood.start} — ${label.title}`}
                  className="fe-spectrum__cta fe-press absolute bottom-4 left-[1.125rem] bg-white/[0.14] hover:bg-white/[0.22]"
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
        className="fe-spectrum__ruler fe-ink-3 fe-mono text-[0.625rem] tracking-[0.08em] uppercase"
        aria-hidden="true"
      >
        <span>40 {copy.mood.bpm}</span>
        <span>{copy.mood.tempoAxis}</span>
        <span>170 {copy.mood.bpm}</span>
      </div>
    </section>
  );
}

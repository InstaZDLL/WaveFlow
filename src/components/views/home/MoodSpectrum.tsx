import { useTranslation } from "react-i18next";
import { Play } from "lucide-react";
import type { Mood, MoodCounts } from "../../../lib/tauri/moodRadio";
import { MOODS, useMoodRadioGroup } from "./moodRadioGroup";

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
 * Selection and the keyboard contract (roving focus, Enter or Space to
 * start) come from `useMoodRadioGroup`, shared with Liquid's capsules.
 */
export function MoodSpectrum({
  counts,
  loadingMood,
  onStart,
}: MoodSpectrumProps) {
  const { t } = useTranslation();
  const { current, setSelected, refs, paths, onKeyDown } = useMoodRadioGroup(
    counts,
    onStart,
    60,
  );

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

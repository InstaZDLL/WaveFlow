import { useTranslation } from "react-i18next";
import { Play } from "lucide-react";
import type { Mood, MoodCounts } from "../../../lib/tauri/moodRadio";
import { MOODS, useMoodRadioGroup } from "./moodRadioGroup";

interface MoodTunerProps {
  counts: MoodCounts | null;
  /** The mood whose radio is being built, if any. */
  loadingMood: Mood | null;
  onStart: (mood: Mood) => void;
}

/**
 * Mood Radio as a receiver's tuning dial — the Lounge skin's drawing of
 * it, where Fluid draws one tempo band and Liquid five capsules. The
 * dial is a strip of graduations with the five moods as stations along
 * it, slowest on the left; choosing one swings the needle onto it, and
 * the readout beside the dial says what the station plays — the tempo
 * window, the line that describes it, the mood's wave, how many tracks
 * qualify — with the one button that starts it. The stations are the
 * radios; the start button is the readout's, outside the radiogroup.
 * Selection and the keyboard contract come from `useMoodRadioGroup`,
 * shared with the spectrum and the capsules.
 */
export function MoodTuner({ counts, loadingMood, onStart }: MoodTunerProps) {
  const { t } = useTranslation();
  const { current, setSelected, refs, paths, onKeyDown } = useMoodRadioGroup(
    counts,
    onStart,
    40,
  );
  const index = MOODS.findIndex((m) => m.id === current);
  const station = index >= 0 ? MOODS[index] : null;
  const count = station ? (counts?.[station.id] ?? 0) : 0;
  const title = station ? t(`home.moodRadio.${station.id}.title`) : "";

  return (
    <div className="fe-tuner">
      <div className="fe-tuner__dial">
        <div className="fe-tuner__scale" aria-hidden="true" />
        <div
          className="fe-tuner__needle"
          aria-hidden="true"
          data-tuned={station != null || undefined}
          style={{
            ["--fe-tuner-pos" as string]:
              index >= 0 ? (index + 0.5) / MOODS.length : 0.5,
          }}
        />
        <div
          className="fe-tuner__stations"
          role="radiogroup"
          aria-label={t("home.moodRadio.title")}
        >
          {MOODS.map((m) => {
            const n = counts?.[m.id] ?? 0;
            const disabled = counts != null && n === 0;
            const isSel = current === m.id;
            const name = t(`home.moodRadio.${m.id}.title`);
            const countLabel = disabled
              ? t("home.moodRadio.empty")
              : t("home.moodRadio.trackCount", { count: n });
            return (
              <button
                key={m.id}
                ref={(el) => {
                  refs.current[m.id] = el;
                }}
                type="button"
                role="radio"
                aria-checked={isSel}
                aria-label={`${name}, ${m.bpm[0]}–${m.bpm[1]} BPM, ${countLabel}`}
                tabIndex={isSel ? 0 : -1}
                disabled={disabled}
                data-checked={isSel}
                onClick={() => setSelected(m.id)}
                onKeyDown={(e) => onKeyDown(e, m)}
                className="fe-tuner__station"
              >
                <span className="fe-tuner__name">{name}</span>
              </button>
            );
          })}
        </div>
      </div>
      <div className="fe-tuner__readout">
        {station && (
          <>
            <svg
              className="fe-tuner__wave"
              viewBox="0 0 400 40"
              preserveAspectRatio="none"
              aria-hidden="true"
            >
              <path
                d={paths[station.id]}
                fill="none"
                stroke="currentColor"
                strokeWidth={1.6}
                vectorEffect="non-scaling-stroke"
              />
            </svg>
            <p className="fe-label fe-mono">
              {station.bpm[0]}–{station.bpm[1]} BPM
            </p>
            <p className="fe-title fe-tuner__title">{title}</p>
            <p className="fe-tuner__sub">
              {t(`home.moodRadio.${station.id}.subtitle`)}
            </p>
            <div className="fe-tuner__foot">
              <button
                type="button"
                onClick={() => onStart(station.id)}
                disabled={loadingMood != null}
                aria-label={`${t("home.fluid.moodStart")} — ${title}`}
                className="fe-tuner__cta fe-press"
              >
                <Play size={12} className="fill-current" strokeWidth={0} />
                {loadingMood === station.id
                  ? t("home.moodRadio.loading")
                  : t("home.fluid.moodStart")}
              </button>
              <span className="fe-mono fe-tuner__count">
                {t("home.moodRadio.trackCount", { count })}
              </span>
            </div>
          </>
        )}
      </div>
      <div
        className="fe-tuner__ruler fe-ink-3 fe-mono text-[0.6875rem] tracking-[0.08em] uppercase"
        aria-hidden="true"
      >
        <span>40 BPM</span>
        <span>{t("home.fluid.tempo")}</span>
        <span>170 BPM</span>
      </div>
    </div>
  );
}

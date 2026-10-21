import { useTranslation } from "react-i18next";
import { Play } from "lucide-react";
import type { Mood, MoodCounts } from "../../../lib/tauri/moodRadio";
import { MOODS, useMoodRadioGroup } from "./moodRadioGroup";

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
 * and clickable; nothing depends on reading a colour. Selection and
 * the keyboard contract come from `useMoodRadioGroup`, shared with the
 * spectrum.
 */
export function MoodCapsules({
  counts,
  loadingMood,
  onStart,
}: MoodCapsulesProps) {
  const { t } = useTranslation();
  const { current, setSelected, refs, paths, onKeyDown } = useMoodRadioGroup(
    counts,
    onStart,
    40,
  );

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

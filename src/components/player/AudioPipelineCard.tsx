import type { ReactNode } from "react";
import type { LucideIcon } from "lucide-react";
import { useTranslation } from "react-i18next";

/**
 * What a stage does to the sound, as the listener would put it.
 *
 * - `none` — the samples are not changed (native DSD, an untouched pass).
 * - `inaudible` — converted, with a difference nobody hears (resampling,
 *   DSD to PCM).
 * - `level` — only the loudness moves (ReplayGain, normalisation, the
 *   volume).
 * - `changes` — the sound itself is changed, on purpose (EQ, mono,
 *   balance, width, speed, pitch, a downmix).
 */
export type StageImpact = "none" | "inaudible" | "level" | "changes";

const IMPACT_TONE: Record<StageImpact, string> = {
  none: "bg-emerald-500/12 text-emerald-700 dark:text-emerald-400",
  inaudible: "bg-sky-500/12 text-sky-700 dark:text-sky-400",
  level: "bg-sky-500/12 text-sky-700 dark:text-sky-400",
  changes: "bg-amber-500/15 text-amber-700 dark:text-amber-400",
};

interface AudioPipelineCardProps {
  icon: LucideIcon;
  title: string;
  /** A short line under the title: the device, the kind of file. */
  subtitle?: string | null;
  /** What happens, in plain words. */
  body?: string | null;
  /** The figures, second in line: rates, depths, percentages. */
  figures?: string | null;
  impact?: StageImpact | null;
  /** A quality badge beside the title (the file's card). */
  badge?: ReactNode;
}

/**
 * One stage of the audio pipeline as a card: an icon, what the stage
 * is, a sentence saying what happens and whether the sound changes, and
 * the figures under it in a quieter ink. `data-pipeline-card` is what
 * the fluid materials hang their surface on.
 */
export function AudioPipelineCard({
  icon: Icon,
  title,
  subtitle,
  body,
  figures,
  impact,
  badge,
}: AudioPipelineCardProps) {
  const { t } = useTranslation();
  return (
    <li
      data-pipeline-card
      className="flex gap-3 rounded-xl border border-zinc-200/80 bg-zinc-50 p-3 dark:border-zinc-800 dark:bg-zinc-800/40"
    >
      <Icon
        size={16}
        className="mt-0.5 shrink-0 text-zinc-400"
        aria-hidden="true"
      />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-baseline justify-between gap-x-2 gap-y-1">
          <span className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-white">
            {title}
            {badge}
          </span>
          {impact && (
            <span
              data-pipeline-impact={impact}
              className={`rounded-full px-2 py-0.5 text-[10px] font-semibold ${IMPACT_TONE[impact]}`}
            >
              {t(`playerBar.pipeline.impact.${impact}`)}
            </span>
          )}
        </div>
        {subtitle && (
          <div className="truncate text-xs text-zinc-600 dark:text-zinc-300">
            {subtitle}
          </div>
        )}
        {body && (
          <p className="mt-1 text-xs leading-relaxed text-zinc-600 dark:text-zinc-300">
            {body}
          </p>
        )}
        {figures && (
          <div
            data-pipeline-figures
            className="mt-1 truncate text-[11px] tabular-nums text-zinc-500 dark:text-zinc-400"
          >
            {figures}
          </div>
        )}
      </div>
    </li>
  );
}

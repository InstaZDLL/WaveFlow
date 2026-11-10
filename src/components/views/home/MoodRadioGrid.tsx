import { useEffect, useState, type CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import {
  Brain,
  Coffee,
  Dumbbell,
  Sparkles,
  Moon,
  SlidersHorizontal,
  Loader2,
} from "lucide-react";
import {
  startMoodRadio,
  moodRadioCounts,
  MOOD_OVERRIDES_EVENT,
  type Mood,
  type MoodCounts,
} from "../../../lib/tauri/moodRadio";
import { playerPlayTracks } from "../../../lib/tauri/player";
import { useSkin } from "../../../hooks/useSkin";
import { MoodCurationModal } from "./MoodCurationModal";
import { MoodSpectrum } from "./MoodSpectrum";
import { SectionHeader } from "./fluid/SectionHeader";
import { MoodCapsules } from "./MoodCapsules";
import { MoodTuner } from "./MoodTuner";

interface MoodTile {
  mood: Mood;
  icon: typeof Brain;
  /** The mood's one colour, a Tailwind palette variable: the tint of the
   *  tile, its icon and its wave. Everything else on the tile is ink. */
  color: string;
  /** How fast the tile's static wave swings — calm moods slow, lively
   *  ones fast — so the five read apart at a glance. */
  waveCycles: number;
}

const TILES: MoodTile[] = [
  {
    mood: "focus",
    icon: Brain,
    color: "var(--color-violet-500)",
    waveCycles: 2,
  },
  {
    mood: "chill",
    icon: Coffee,
    color: "var(--color-amber-500)",
    waveCycles: 1.5,
  },
  {
    mood: "workout",
    icon: Dumbbell,
    color: "var(--color-rose-500)",
    waveCycles: 4,
  },
  {
    mood: "party",
    icon: Sparkles,
    color: "var(--color-fuchsia-500)",
    waveCycles: 5,
  },
  {
    mood: "sleep",
    icon: Moon,
    color: "var(--color-sky-500)",
    waveCycles: 1,
  },
];

const WAVE_BARS = 14;

/** The bar heights of a tile's wave, computed once per mood: a sine at
 *  the mood's pace under a soft envelope. Drawn as a static SVG — it
 *  never moves. */
function waveHeights(cycles: number): number[] {
  return Array.from({ length: WAVE_BARS }, (_, i) => {
    const x = i / (WAVE_BARS - 1);
    const swing = Math.abs(Math.sin(x * Math.PI * cycles + 0.4));
    return 0.25 + 0.75 * swing * (0.55 + 0.45 * Math.sin(x * Math.PI));
  });
}

const WAVES: Record<Mood, number[]> = Object.fromEntries(
  TILES.map(({ mood, waveCycles }) => [mood, waveHeights(waveCycles)]),
) as Record<Mood, number[]>;

function MoodWave({ heights }: { heights: number[] }) {
  const width = WAVE_BARS * 4 - 2;
  return (
    <svg
      viewBox={`0 0 ${width} 16`}
      aria-hidden="true"
      className="h-4 w-14 shrink-0 text-[var(--mood)] opacity-70"
    >
      {heights.map((h, i) => (
        <rect
          key={i}
          x={i * 4}
          y={(16 - h * 16) / 2}
          width={2}
          height={h * 16}
          rx={1}
          fill="currentColor"
        />
      ))}
    </svg>
  );
}

export function MoodRadioGrid() {
  const { t } = useTranslation();
  const { skin } = useSkin();
  const [counts, setCounts] = useState<MoodCounts | null>(null);
  const [loadingMood, setLoadingMood] = useState<Mood | null>(null);
  const [curating, setCurating] = useState(false);
  // Bumped when a track is put into a mood or taken out of one by hand
  // — from a track menu as much as from the moods dialog — so the tiles
  // count again, and a mood that just gained its first track lights up.
  const [countsVersion, setCountsVersion] = useState(0);
  useEffect(() => {
    const bump = () => setCountsVersion((n) => n + 1);
    window.addEventListener(MOOD_OVERRIDES_EVENT, bump);
    return () => window.removeEventListener(MOOD_OVERRIDES_EVENT, bump);
  }, []);
  const isFluidLayout = skin.layout === "fluid";

  useEffect(() => {
    let cancelled = false;
    moodRadioCounts()
      .then((c) => {
        if (!cancelled) setCounts(c);
      })
      .catch((err) => console.error("[MoodRadioGrid] counts failed", err));
    return () => {
      cancelled = true;
    };
  }, [countsVersion]);

  const handleStart = async (mood: Mood) => {
    if (loadingMood != null) return;
    setLoadingMood(mood);
    try {
      const session = await startMoodRadio(mood);
      if (session.trackIds.length === 0) return;
      await playerPlayTracks(
        "radio",
        null,
        session.trackIds,
        0,
        session.albumOrdered,
      );
    } catch (err) {
      console.error("[MoodRadioGrid] start mood radio failed", err);
    } finally {
      setLoadingMood(null);
    }
  };

  const eligible = counts
    ? counts.focus + counts.chill + counts.workout + counts.party + counts.sleep
    : 0;
  // When no mood matches anything, the library either has no BPM
  // analysis at all or only a handful of analysed tracks. In that
  // case we hide the section entirely instead of showing a row of
  // disabled tiles — feels less broken.
  // Unless tracks were put into or taken out of a mood by hand: then
  // the section stays, so the dialog that undoes them stays reachable.
  if (counts != null && eligible === 0 && counts.overrides === 0) return null;

  // Every mood draws from the analysed part of the library, so a
  // partly-analysed one gives thin radios for a reason the tiles
  // cannot show: their counts look small without saying small *of
  // what*. Shown only while the two numbers differ — once everything
  // is analysed the line has nothing to add.
  const partlyAnalysed =
    counts != null && counts.analysed_tracks < counts.total_tracks;

  if (isFluidLayout) {
    return (
      <section aria-label={t("home.moodRadio.title")}>
        <SectionHeader
          title={t("home.moodRadio.title")}
          className="mb-5"
          lede={
            <>
              {t("home.moodRadio.subtitle")}
              {partlyAnalysed && (
                <>
                  <span className="mx-1.5 opacity-50">·</span>
                  {t("home.moodRadio.coverage", {
                    count: counts.total_tracks,
                    analysed: counts.analysed_tracks,
                  })}
                </>
              )}
            </>
          }
          action={
            <button
              type="button"
              onClick={() => setCurating(true)}
              title={t("moodCuration.open")}
              aria-label={t("moodCuration.open")}
              className="fe-icon-btn"
            >
              <SlidersHorizontal size={16} />
            </button>
          }
        />
        <MoodCurationModal
          isOpen={curating}
          onClose={() => setCurating(false)}
        />
        {skin.fluid?.moodRadio === "capsules" ? (
          <MoodCapsules
            counts={counts}
            loadingMood={loadingMood}
            onStart={handleStart}
          />
        ) : skin.fluid?.moodRadio === "tuner" ? (
          <MoodTuner
            counts={counts}
            loadingMood={loadingMood}
            onStart={handleStart}
          />
        ) : (
          <MoodSpectrum
            counts={counts}
            loadingMood={loadingMood}
            onStart={handleStart}
          />
        )}
      </section>
    );
  }

  return (
    <section>
      <div className="mb-4 flex items-center justify-between gap-4">
        <h2 className="text-lg font-semibold tracking-[-0.01em] text-zinc-900 dark:text-white">
          {t("home.moodRadio.title")}
        </h2>
        <div className="flex items-center gap-3">
          <span className="text-end text-xs text-zinc-500 dark:text-zinc-400">
            {t("home.moodRadio.subtitle")}
            {partlyAnalysed && (
              <>
                <br />
                {/* `count` is the total, because that is the number the
                  sentence's noun agrees with — Russian and Arabic
                  inflect "tracks" on it, and a fixed form is wrong for
                  most of the values this line actually shows. */}
                {t("home.moodRadio.coverage", {
                  count: counts.total_tracks,
                  analysed: counts.analysed_tracks,
                })}
              </>
            )}
          </span>
          <button
            type="button"
            onClick={() => setCurating(true)}
            title={t("moodCuration.open")}
            aria-label={t("moodCuration.open")}
            className="shrink-0 rounded-lg p-2 text-zinc-500 transition-colors hover:bg-zinc-100 hover:text-zinc-800 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 dark:text-zinc-400 dark:hover:bg-zinc-800 dark:hover:text-zinc-200"
          >
            <SlidersHorizontal size={16} />
          </button>
        </div>
      </div>
      <MoodCurationModal isOpen={curating} onClose={() => setCurating(false)} />
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-5">
        {TILES.map(({ mood, icon: Icon, color }) => {
          const count = counts?.[mood] ?? 0;
          const disabled = counts != null && count === 0;
          const isLoading = loadingMood === mood;
          return (
            <button
              key={mood}
              type="button"
              onClick={() => handleStart(mood)}
              disabled={disabled || isLoading}
              aria-busy={isLoading || undefined}
              style={{ "--mood": color } as CSSProperties}
              className="group flex min-w-0 flex-col rounded-xl border border-zinc-200 bg-[color-mix(in_srgb,var(--mood)_5%,white)] p-4 text-start transition-colors hover:border-[color-mix(in_srgb,var(--mood)_45%,transparent)] focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:border-zinc-200 dark:border-zinc-800 dark:bg-[color-mix(in_srgb,var(--mood)_7%,var(--color-zinc-900))] dark:disabled:hover:border-zinc-800"
              aria-label={t(`home.moodRadio.${mood}.title`)}
            >
              <span className="mb-3 flex items-center justify-between gap-2">
                <span className="flex h-8 w-8 items-center justify-center rounded-lg bg-[color-mix(in_srgb,var(--mood)_14%,transparent)] text-[color-mix(in_srgb,var(--mood)_80%,black)] dark:text-[color-mix(in_srgb,var(--mood)_70%,white)]">
                  {isLoading ? (
                    <Loader2
                      size={16}
                      className="animate-spin"
                      aria-hidden="true"
                    />
                  ) : (
                    <Icon size={16} aria-hidden="true" />
                  )}
                </span>
                <MoodWave heights={WAVES[mood]} />
              </span>
              <span className="mb-0.5 text-sm font-semibold text-zinc-900 dark:text-white">
                {t(`home.moodRadio.${mood}.title`)}
              </span>
              <span className="line-clamp-2 min-h-8 text-xs text-zinc-600 dark:text-zinc-300">
                {t(`home.moodRadio.${mood}.subtitle`)}
              </span>
              <span className="mt-3 text-[11px] font-medium tabular-nums text-zinc-500 dark:text-zinc-400">
                {disabled
                  ? t("home.moodRadio.empty")
                  : isLoading
                    ? t("home.moodRadio.loading")
                    : t("home.moodRadio.trackCount", { count })}
              </span>
            </button>
          );
        })}
      </div>
    </section>
  );
}

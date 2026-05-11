import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Brain, Coffee, Dumbbell, Sparkles, Moon } from "lucide-react";
import {
  startMoodRadio,
  moodRadioCounts,
  type Mood,
  type MoodCounts,
} from "../../../lib/tauri/moodRadio";
import { playerPlayTracks } from "../../../lib/tauri/player";

interface MoodTile {
  mood: Mood;
  icon: typeof Brain;
  /** Tailwind classes for the card surface (light + dark). */
  cardClass: string;
  /** Tailwind classes for the icon chip. */
  iconClass: string;
}

const TILES: MoodTile[] = [
  {
    mood: "focus",
    icon: Brain,
    cardClass:
      "bg-gradient-to-br from-violet-500 to-indigo-600 dark:from-violet-700 dark:to-indigo-800",
    iconClass: "bg-white/15 text-white",
  },
  {
    mood: "chill",
    icon: Coffee,
    cardClass:
      "bg-gradient-to-br from-amber-400 to-orange-500 dark:from-amber-600 dark:to-orange-700",
    iconClass: "bg-white/15 text-white",
  },
  {
    mood: "workout",
    icon: Dumbbell,
    cardClass:
      "bg-gradient-to-br from-rose-500 to-pink-600 dark:from-rose-700 dark:to-pink-800",
    iconClass: "bg-white/15 text-white",
  },
  {
    mood: "party",
    icon: Sparkles,
    cardClass:
      "bg-gradient-to-br from-fuchsia-500 to-purple-600 dark:from-fuchsia-700 dark:to-purple-800",
    iconClass: "bg-white/15 text-white",
  },
  {
    mood: "sleep",
    icon: Moon,
    cardClass:
      "bg-gradient-to-br from-sky-600 to-slate-700 dark:from-sky-800 dark:to-slate-900",
    iconClass: "bg-white/15 text-white",
  },
];

export function MoodRadioGrid() {
  const { t } = useTranslation();
  const [counts, setCounts] = useState<MoodCounts | null>(null);
  const [loadingMood, setLoadingMood] = useState<Mood | null>(null);

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
  }, []);

  const handleStart = async (mood: Mood) => {
    if (loadingMood != null) return;
    setLoadingMood(mood);
    try {
      const ids = await startMoodRadio(mood);
      if (ids.length === 0) return;
      await playerPlayTracks("radio", null, ids, 0);
    } catch (err) {
      console.error("[MoodRadioGrid] start mood radio failed", err);
    } finally {
      setLoadingMood(null);
    }
  };

  const totalAnalysed = counts
    ? counts.focus + counts.chill + counts.workout + counts.party + counts.sleep
    : 0;
  // When no mood matches anything, the library either has no BPM
  // analysis at all or only a handful of analysed tracks. In that
  // case we hide the section entirely instead of showing a row of
  // disabled tiles — feels less broken.
  if (counts != null && totalAnalysed === 0) return null;

  return (
    <section>
      <div className="flex items-end justify-between mb-6">
        <h2 className="text-2xl font-bold inline-block border-b-4 border-rose-500 pb-1 text-zinc-900 dark:text-white">
          {t("home.moodRadio.title")}
        </h2>
        <span className="text-xs text-zinc-500 dark:text-zinc-400">
          {t("home.moodRadio.subtitle")}
        </span>
      </div>
      <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-5 gap-4">
        {TILES.map(({ mood, icon: Icon, cardClass, iconClass }) => {
          const count = counts?.[mood] ?? 0;
          const disabled = counts != null && count === 0;
          const isLoading = loadingMood === mood;
          return (
            <button
              key={mood}
              type="button"
              onClick={() => handleStart(mood)}
              disabled={disabled || isLoading}
              className={`relative overflow-hidden rounded-2xl p-5 text-left text-white shadow-sm transition-transform hover:-translate-y-0.5 hover:shadow-lg disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:translate-y-0 disabled:hover:shadow-sm ${cardClass}`}
              aria-label={t(`home.moodRadio.${mood}.title`)}
            >
              <div
                className={`inline-flex items-center justify-center w-10 h-10 rounded-xl mb-3 ${iconClass}`}
              >
                <Icon size={20} />
              </div>
              <div className="text-base font-semibold mb-1">
                {t(`home.moodRadio.${mood}.title`)}
              </div>
              <div className="text-xs text-white/80 line-clamp-2 min-h-8">
                {t(`home.moodRadio.${mood}.subtitle`)}
              </div>
              <div className="mt-3 text-[11px] font-medium text-white/70">
                {disabled
                  ? t("home.moodRadio.empty")
                  : isLoading
                    ? t("home.moodRadio.loading")
                    : t("home.moodRadio.trackCount", { count })}
              </div>
            </button>
          );
        })}
      </div>
    </section>
  );
}

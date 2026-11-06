import { useTranslation } from "react-i18next";
import { Loader2, Play, Shuffle } from "lucide-react";
import { Artwork } from "../../common/Artwork";
import { useHomeSuggestion } from "./useHomeSuggestion";

/**
 * Home's suggestion of the day, as Studio draws it: a flat card with the
 * cover at its start, the kind as a kicker, the reason under the title,
 * "Another suggestion" and Play on the right. No blurred cover behind
 * it: Studio's surfaces stay flat (`docs/design/studio-refined.md`).
 * The draw itself — stable for the day, remembered per profile — lives
 * in [`useHomeSuggestion`](./useHomeSuggestion.ts), shared with the
 * Fluid skin's card.
 */
export function HomeSuggestionCard() {
  const { t } = useTranslation();
  const view = useHomeSuggestion();
  if (!view) return null;
  const { track, kindLabel, reason, starting, play, another } = view;

  const subtitle = [track.artist_name, track.album_title]
    .filter(Boolean)
    .join(" · ");

  return (
    <section
      aria-label={t("home.suggestion.label")}
      className="rounded-xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-900"
    >
      <div className="flex flex-wrap items-center gap-4 p-4">
        <Artwork
          path={track.artwork_path}
          path1x={track.artwork_path_1x}
          path2x={track.artwork_path_2x}
          size="2x"
          alt=""
          className="h-16 w-16 shrink-0"
          iconSize={28}
          rounded="lg"
        />
        <div className="min-w-48 flex-1">
          <p className="mb-0.5 text-[11px] font-semibold uppercase tracking-[0.08em] text-emerald-600 dark:text-emerald-400">
            {kindLabel}
          </p>
          <h2 className="truncate text-base font-semibold text-zinc-900 dark:text-white">
            {track.title}
          </h2>
          {subtitle && (
            <p className="truncate text-sm text-zinc-600 dark:text-zinc-300">
              {subtitle}
            </p>
          )}
          {reason && (
            <p className="mt-1 truncate text-xs text-zinc-500 dark:text-zinc-400">
              {reason}
            </p>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <button
            type="button"
            onClick={another}
            className="inline-flex h-8 items-center gap-2 rounded-lg px-3 text-sm font-medium text-zinc-600 transition-colors hover:bg-zinc-100 hover:text-zinc-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 dark:text-zinc-300 dark:hover:bg-white/5 dark:hover:text-white"
          >
            <Shuffle size={15} aria-hidden="true" />
            <span>{t("home.suggestion.another")}</span>
          </button>
          <button
            type="button"
            onClick={() => void play()}
            disabled={starting}
            aria-busy={starting}
            aria-label={t("home.suggestion.playAria", { title: track.title })}
            className="inline-flex h-8 items-center gap-2 rounded-lg bg-emerald-600 px-3 text-sm font-medium text-white transition-colors hover:bg-emerald-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 focus-visible:ring-offset-2 focus-visible:ring-offset-white disabled:opacity-70 dark:bg-emerald-500 dark:text-zinc-950 dark:hover:bg-emerald-400 dark:focus-visible:ring-offset-zinc-900"
          >
            {starting ? (
              <Loader2
                size={15}
                className="animate-spin motion-reduce:animate-none"
                aria-hidden="true"
              />
            ) : (
              <Play size={15} className="fill-current" aria-hidden="true" />
            )}
            <span>{t("home.suggestion.play")}</span>
          </button>
        </div>
      </div>
    </section>
  );
}

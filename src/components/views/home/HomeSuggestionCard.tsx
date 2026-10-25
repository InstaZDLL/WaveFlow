import { useTranslation } from "react-i18next";
import { Loader2, Play, Shuffle } from "lucide-react";
import { Artwork } from "../../common/Artwork";
import { PrecomputedBlur } from "../../common/PrecomputedBlur";
import { resolveArtwork } from "../../../lib/tauri/artwork";
import { useHomeSuggestion } from "./useHomeSuggestion";

/**
 * Home's suggestion of the day, as the classic skins draw it: the track
 * over its blurred cover, the kind as a kicker, the reason under the
 * title, "Another suggestion" and Play on the right. The draw itself —
 * stable for the day, remembered per profile — lives in
 * [`useHomeSuggestion`](./useHomeSuggestion.ts), shared with the Fluid
 * skin's card.
 */
export function HomeSuggestionCard() {
  const { t } = useTranslation();
  const view = useHomeSuggestion();
  if (!view) return null;
  const { track, kindLabel, reason, starting, play, another } = view;

  // The blur keeps no detail: the 64 px thumbnail is enough.
  const blurSrc = resolveArtwork(
    {
      full: track.artwork_path,
      x1: track.artwork_path_1x ?? null,
      x2: track.artwork_path_2x ?? null,
    },
    "1x",
  );
  const subtitle = [track.artist_name, track.album_title]
    .filter(Boolean)
    .join(" · ");

  return (
    <section
      aria-label={t("home.suggestion.label")}
      className="relative overflow-hidden rounded-3xl bg-zinc-900 text-white"
    >
      {/* The cover, blurred once rather than once per frame. */}
      {blurSrc && (
        <div
          aria-hidden="true"
          className="pointer-events-none absolute inset-0 opacity-70"
        >
          <PrecomputedBlur key={blurSrc} src={blurSrc} blur={40} scale={1.25} />
        </div>
      )}
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 bg-linear-to-r from-black/75 via-black/55 to-black/35"
      />
      <div className="relative flex flex-wrap items-center gap-5 p-5 md:p-6">
        <Artwork
          path={track.artwork_path}
          path1x={track.artwork_path_1x}
          path2x={track.artwork_path_2x}
          size="2x"
          alt=""
          className="w-20 h-20 md:w-24 md:h-24 shrink-0 shadow-xl"
          iconSize={32}
          rounded="xl"
        />
        <div className="flex-1 min-w-48">
          <p className="uppercase tracking-[0.3em] text-xs font-semibold text-white/70 mb-1">
            {kindLabel}
          </p>
          <h2 className="text-xl font-bold truncate">{track.title}</h2>
          {subtitle && (
            <p className="text-sm text-white/70 truncate">{subtitle}</p>
          )}
          {reason && (
            <p className="mt-2 text-sm text-white/85 truncate">{reason}</p>
          )}
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <button
            type="button"
            onClick={another}
            className="inline-flex items-center gap-2 px-3 py-2.5 rounded-full text-sm font-medium text-white/85 hover:text-white hover:bg-white/15 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-white/80"
          >
            <Shuffle size={16} aria-hidden="true" />
            <span>{t("home.suggestion.another")}</span>
          </button>
          <button
            type="button"
            onClick={() => void play()}
            disabled={starting}
            aria-busy={starting}
            aria-label={t("home.suggestion.playAria", { title: track.title })}
            className="inline-flex items-center gap-2 px-4 py-2.5 rounded-full bg-white text-zinc-900 font-semibold text-sm transition-transform hover:scale-105 disabled:opacity-80 disabled:hover:scale-100 motion-reduce:transition-none motion-reduce:hover:scale-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-white/80 focus-visible:ring-offset-2 focus-visible:ring-offset-zinc-900"
          >
            {starting ? (
              <Loader2
                size={16}
                className="animate-spin motion-reduce:animate-none"
                aria-hidden="true"
              />
            ) : (
              <Play size={16} className="fill-current" aria-hidden="true" />
            )}
            <span>{t("home.suggestion.play")}</span>
          </button>
        </div>
      </div>
    </section>
  );
}

import type { CSSProperties } from "react";
import { Loader2, Play, Shuffle } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Artwork } from "../../../common/Artwork";
import { ArtistLink } from "../../../common/ArtistLink";
import { PrecomputedBlur } from "../../../common/PrecomputedBlur";
import { useHomeSuggestion } from "../useHomeSuggestion";
import { FlowLine } from "./SectionHeader";
import { blurSourceUrl } from "./format";

interface SuggestionOfTheDayProps {
  onNavigateToArtist: (artistId: number) => void;
  /** The page's stagger index (`--i`), applied here since the band may
   *  render nothing. */
  style?: CSSProperties;
}

/**
 * The suggestion of the day under the Fluid skin: one band on the page's
 * surface, the cover on the left with its own light spilling behind it,
 * the kind in the editorial italic (the voice that says "you used to
 * love this"), the track in the title face, and the reason as a quiet
 * line. The same draw as the classic card, through `useHomeSuggestion`.
 */
export function SuggestionOfTheDay({
  onNavigateToArtist,
  style,
}: SuggestionOfTheDayProps) {
  const { t } = useTranslation();
  const view = useHomeSuggestion();
  if (!view) return null;
  const { track, kindLabel, reason, starting, play, another } = view;
  const blurSrc = blurSourceUrl(track);

  return (
    <section
      aria-label={t("home.suggestion.label")}
      className="fe-suggest fe-rise relative isolate overflow-hidden rounded-[1.125rem] bg-[var(--fe-surface)]"
      style={style}
    >
      {/* The cover's light, under the left half of the band. Keyed on the
          URL so another suggestion cross-fades in. Darkened to half before
          the blur: a light cover at a fifth of its opacity raised the
          surface from 18 to 74 in luminance, too close to the tertiary
          ink; at half brightness it stays under 45. */}
      <div
        key={blurSrc ?? "none"}
        className="fe-suggest__aura"
        aria-hidden="true"
      >
        {blurSrc && (
          <PrecomputedBlur
            src={blurSrc}
            blur={72}
            brightness={0.5}
            saturate={1.1}
            scale={1.2}
          />
        )}
      </div>

      <div className="relative flex flex-wrap items-center gap-x-7 gap-y-5 px-6 py-5 md:px-7 md:py-6">
        <Artwork
          path={track.artwork_path}
          path1x={track.artwork_path_1x}
          path2x={track.artwork_path_2x}
          // One cover at 128 px over a 125 % display outgrows the 2×
          // thumbnail; the full file costs nothing for a single tile.
          size="full"
          alt=""
          className="fe-tile h-28 w-28 shrink-0 md:h-32 md:w-32"
          iconSize={32}
          rounded="xl"
        />

        <div className="min-w-[16rem] flex-1">
          <p className="flex items-center gap-3">
            <span className="fe-label">{t("home.suggestion.label")}</span>
            <FlowLine className="h-2 w-7" />
          </p>
          <p className="fe-editorial fe-ink-2 mt-2 text-[1.375rem] leading-tight">
            {kindLabel}
          </p>
          <h2 className="fe-title fe-ink mt-1.5 truncate text-[1.625rem]">
            {track.title}
          </h2>
          <p className="fe-ink-2 mt-1 truncate text-[0.9375rem]">
            <ArtistLink
              name={track.artist_name}
              artistIds={track.artist_ids}
              onNavigate={onNavigateToArtist}
              fallback="—"
            />
            {track.album_title && track.album_title !== track.title && (
              <>
                <span className="mx-2 opacity-50">·</span>
                {track.album_title}
              </>
            )}
          </p>
          {reason && (
            <p className="fe-ink-3 mt-2.5 truncate text-[0.8125rem]">
              {reason}
            </p>
          )}
        </div>

        <div className="flex shrink-0 flex-wrap items-center gap-x-6 gap-y-3">
          <button
            type="button"
            onClick={another}
            className="fe-link inline-flex items-center gap-2 text-[0.875rem]"
          >
            <Shuffle size={14} strokeWidth={1.75} aria-hidden="true" />
            <span>{t("home.suggestion.another")}</span>
          </button>
          <button
            type="button"
            className="fe-cta disabled:opacity-80"
            onClick={() => void play()}
            disabled={starting}
            aria-busy={starting}
            aria-label={t("home.suggestion.playAria", { title: track.title })}
          >
            <span className="fe-cta__icon">
              {starting ? (
                <Loader2
                  size={14}
                  className="animate-spin motion-reduce:animate-none"
                  aria-hidden="true"
                />
              ) : (
                <Play
                  size={14}
                  className="translate-x-px fill-current"
                  strokeWidth={0}
                  aria-hidden="true"
                />
              )}
            </span>
            <span>{t("home.suggestion.play")}</span>
          </button>
        </div>
      </div>
    </section>
  );
}

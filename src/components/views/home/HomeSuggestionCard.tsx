import { useEffect, useRef, useState, type CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, Play, Shuffle } from "lucide-react";
import { useProfile } from "../../../hooks/useProfile";
import { useSuggestionDraw } from "../../../hooks/useSuggestionDraw";
import {
  getHomeSuggestion,
  localDayIndex,
  pinSuggestion,
  type HomeSuggestion,
} from "../../../lib/tauri/homeSuggestion";
import { playerPlayTracks } from "../../../lib/tauri/player";
import { startRadio } from "../../../lib/tauri/radio";
import { Artwork } from "../../common/Artwork";

/**
 * How many numbered wordings each line has in the locales
 * (`home.suggestion.kind.forgotten1` … `3`, `reason.vibe1` … `3`, …).
 * Every locale carries every variant.
 */
const KIND_VARIANTS = 3;
const REASON_VARIANTS = {
  forgotten: 2,
  vibe: 3,
  unplayedArtist: 2,
  unplayedNew: 2,
} as const;

/**
 * Home's suggestion of the day: one track and the reason it was picked —
 * a favourite not played for a while, a track in line with the recent
 * listening, or one never played (`waveflow_core::home_suggestion`).
 *
 * The draw is stable for the day: "Another suggestion" moves to the next
 * one and is remembered per profile, so the card does not change on every
 * visit. Play starts the track followed by tracks like it, through the
 * same seeded radio as the track menu's "Start radio".
 */
export function HomeSuggestionCard({
  clipStyle,
}: {
  /** The rounded-corner mask Home gives its blurred banners (Safari). */
  clipStyle: CSSProperties;
}) {
  const { t, i18n } = useTranslation();
  const { activeProfile } = useProfile();
  const profileId = activeProfile?.id ?? null;
  const draw = useSuggestionDraw();
  const setDraw = draw.setValue;

  // The calendar day, moved on at local midnight so a Home left open
  // overnight draws the new day's suggestion.
  const [today, setToday] = useState(() => localDayIndex(new Date()));
  useEffect(() => {
    const now = new Date();
    const midnight = new Date(
      now.getFullYear(),
      now.getMonth(),
      now.getDate() + 1,
    );
    const timer = window.setTimeout(
      () => setToday(localDayIndex(new Date())),
      midnight.getTime() - now.getTime() + 1_000,
    );
    return () => window.clearTimeout(timer);
  }, [today]);

  // A draw from another day is spent: the day starts over at 0, with
  // nothing pinned.
  const sameDay = draw.value.day === today;
  const attempt = sameDay ? draw.value.attempt : 0;
  const pinned = sameDay ? draw.value.pinned : null;

  // Each result carries the profile and the day it was asked for, and
  // shows only while both still hold: after a profile switch the outgoing
  // profile's track must not stay on screen, nor yesterday's after
  // midnight, while the new one loads (or for good if that load fails).
  // A click on "Another suggestion" keeps the current card until the next
  // arrives, rather than hiding it for the length of the call.
  const [loaded, setLoaded] = useState<{
    profileId: number;
    day: number;
    suggestion: HomeSuggestion | null;
  } | null>(null);
  const [starting, setStarting] = useState(false);
  // Bumped by each load and by "Another suggestion": a response that is
  // not the latest request's is dropped at once. The effect's cleanup
  // alone runs only after the next render, and a load landing in between
  // would write its old draw back over the click.
  const requestRef = useRef(0);

  useEffect(() => {
    if (!draw.ready || profileId == null) return;
    let cancelled = false;
    const request = ++requestRef.current;
    const stale = () => cancelled || request !== requestRef.current;
    getHomeSuggestion(today, attempt, pinned)
      .then((next) => {
        if (stale()) return;
        setLoaded({ profileId, day: today, suggestion: next });
        // Keep a new draw, so the same track comes back for the rest of
        // the day even once played (it would no longer qualify).
        if (next && next.track.id !== pinned?.trackId) {
          void setDraw({ day: today, attempt, pinned: pinSuggestion(next) });
        }
      })
      .catch((err) => {
        console.error("[HomeSuggestionCard] suggestion failed", err);
        if (!stale()) setLoaded({ profileId, day: today, suggestion: null });
      });
    return () => {
      cancelled = true;
    };
  }, [draw.ready, profileId, today, attempt, pinned, setDraw]);

  const suggestion =
    loaded && loaded.profileId === profileId && loaded.day === today
      ? loaded.suggestion
      : null;
  if (!suggestion) return null;
  const { track } = suggestion;

  const handlePlay = async () => {
    setStarting(true);
    try {
      const ids = await startRadio(track.id);
      await playerPlayTracks("radio", null, ids.length ? ids : [track.id], 0);
    } catch (err) {
      console.error("[HomeSuggestionCard] play failed", err);
    } finally {
      setStarting(false);
    }
  };

  const handleAnother = () => {
    requestRef.current += 1;
    void setDraw({ day: today, attempt: attempt + 1, pinned: null });
  };

  const monthYear = (ms: number) =>
    new Intl.DateTimeFormat(i18n.language, {
      month: "long",
      year: "numeric",
    }).format(ms);
  const fullDate = (ms: number) =>
    new Intl.DateTimeFormat(i18n.language, { dateStyle: "long" }).format(ms);

  // Which wording to use, among `count` numbered variants: tied to the
  // draw and the track, so a suggestion keeps its words for the day.
  const variant = (salt: number, count: number) =>
    (((((today + attempt) * 7 + track.id * 13 + salt) % count) + count) %
      count) +
    1;

  let reason: string | null = null;
  if (suggestion.kind === "forgotten" && suggestion.plays != null) {
    reason = t(
      `home.suggestion.reason.forgotten${variant(1, REASON_VARIANTS.forgotten)}`,
      {
        count: suggestion.plays,
        date: suggestion.lastPlayedAt ? monthYear(suggestion.lastPlayedAt) : "",
      },
    );
  } else if (suggestion.kind === "vibe" && suggestion.anchorArtist) {
    reason = t(
      `home.suggestion.reason.vibe${variant(2, REASON_VARIANTS.vibe)}`,
      { artist: suggestion.anchorArtist },
    );
  } else if (suggestion.kind === "unplayed") {
    reason =
      (suggestion.artistPlays ?? 0) > 0 && track.artist_name
        ? t(
            `home.suggestion.reason.unplayedArtist${variant(3, REASON_VARIANTS.unplayedArtist)}`,
            { artist: track.artist_name },
          )
        : suggestion.addedAt
          ? t(
              `home.suggestion.reason.unplayedNew${variant(4, REASON_VARIANTS.unplayedNew)}`,
              { date: fullDate(suggestion.addedAt) },
            )
          : null;
  }
  const kindLabel = t(
    `home.suggestion.kind.${suggestion.kind}${variant(0, KIND_VARIANTS)}`,
  );

  const hasArtwork = Boolean(
    track.artwork_path || track.artwork_path_1x || track.artwork_path_2x,
  );
  const subtitle = [track.artist_name, track.album_title]
    .filter(Boolean)
    .join(" · ");

  return (
    <section
      aria-label={t("home.suggestion.label")}
      className="relative overflow-hidden rounded-3xl bg-zinc-900 text-white"
      style={clipStyle}
    >
      {hasArtwork && (
        <div
          aria-hidden="true"
          className="pointer-events-none absolute inset-0 scale-125 blur-2xl opacity-70"
        >
          <Artwork
            path={track.artwork_path}
            path1x={track.artwork_path_1x}
            path2x={track.artwork_path_2x}
            size="1x"
            alt=""
            className="w-full h-full"
          />
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
            onClick={handleAnother}
            className="inline-flex items-center gap-2 px-3 py-2.5 rounded-full text-sm font-medium text-white/85 hover:text-white hover:bg-white/15 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-white/80"
          >
            <Shuffle size={16} aria-hidden="true" />
            <span>{t("home.suggestion.another")}</span>
          </button>
          <button
            type="button"
            onClick={() => void handlePlay()}
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

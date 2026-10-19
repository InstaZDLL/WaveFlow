import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useProfile } from "../../../hooks/useProfile";
import { useSuggestionDraw } from "../../../hooks/useSuggestionDraw";
import {
  getHomeSuggestion,
  localDayIndex,
  pinSuggestion,
  type PinnedSuggestion,
  type HomeSuggestion,
} from "../../../lib/tauri/homeSuggestion";
import { playerPlayTracks } from "../../../lib/tauri/player";
import { startRadio } from "../../../lib/tauri/radio";
import type { Track } from "../../../lib/tauri/track";

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

/** The suggestion of the day, ready to draw: the words and the actions. */
export interface HomeSuggestionView {
  suggestion: HomeSuggestion;
  track: Track;
  /** "You used to love this" — the kind, in one of its wordings. */
  kindLabel: string;
  /** The reason line, or `null` when the kind's data is missing. */
  reason: string | null;
  /** `true` while Play is starting the radio. */
  starting: boolean;
  /** Starts the track followed by tracks like it (the seeded radio). */
  play: () => Promise<void>;
  /** Moves to the next suggestion, remembered for the rest of the day. */
  another: () => void;
}

/**
 * Home's suggestion of the day: one track and the reason it was picked —
 * a favourite not played for a while, a track in line with the recent
 * listening, or one never played (`waveflow_core::home_suggestion`).
 *
 * The draw is stable for the day: "Another suggestion" moves to the next
 * one and is remembered per profile, so the card does not change on every
 * visit. Play starts the track followed by tracks like it, through the
 * same seeded radio as the track menu's "Start radio".
 *
 * Shared by the classic card and the Fluid skin's, which only differ in
 * how they draw it. `null` while loading and when the library has nothing
 * to suggest: both cards then render nothing.
 */
export function useHomeSuggestion(): HomeSuggestionView | null {
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

  // A draw counts only for the profile it was made for: right after a
  // profile switch the hook still holds the outgoing profile's value for
  // a render, and its pinned track id may name another track in the new
  // database. Such a draw waits for the hook's re-read; one from another
  // day is spent, and the day starts over at 0 with nothing pinned.
  const drawProfile = draw.value.profileId;
  const foreignDraw = drawProfile != null && drawProfile !== profileId;
  const sameDay = drawProfile === profileId && draw.value.day === today;
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
  // The suggestion this card drew for a profile, day and click count,
  // kept here as well as in the setting. When storing it fails, the
  // setting hook rolls the draw back; asking again with the stored pin
  // alone would draw anew, try to store that, fail and roll back, over
  // and over for as long as the database refuses writes. With this one
  // handed back instead, the same track returns and nothing is written,
  // so the card keeps what it showed and the loop never starts.
  //
  // `replaces` is the stored pin a draw was made in place of, when the
  // backend found that pin's track gone. A rollback restores exactly that
  // pin, and it must not win again: it would draw anew every time. Any
  // other stored pin does win, so a draw stored by another mounted card
  // is followed rather than overridden.
  const drawnRef = useRef<{
    key: string;
    pin: PinnedSuggestion;
    replaces: number | null;
  } | null>(null);

  useEffect(() => {
    if (!draw.ready || profileId == null || foreignDraw) return;
    let cancelled = false;
    const request = ++requestRef.current;
    const stale = () => cancelled || request !== requestRef.current;
    const key = `${profileId}:${today}:${attempt}`;
    const drawn = drawnRef.current?.key === key ? drawnRef.current : null;
    const pin =
      drawn && (pinned == null || pinned.trackId === drawn.replaces)
        ? drawn.pin
        : pinned;
    getHomeSuggestion(today, attempt, pin)
      .then((next) => {
        if (stale()) return;
        setLoaded({ profileId, day: today, suggestion: next });
        if (next) {
          drawnRef.current = {
            key,
            pin: pinSuggestion(next),
            // A new track in place of the pin that was sent means that
            // pin's track is gone; the same track back keeps what it
            // already replaced.
            replaces:
              next.track.id !== pin?.trackId
                ? (pin?.trackId ?? null)
                : (drawn?.replaces ?? null),
          };
        }
        // Keep a new draw, so the same track comes back for the rest of
        // the day even once played (it would no longer qualify).
        if (next && next.track.id !== pin?.trackId) {
          void setDraw({
            profileId,
            day: today,
            attempt,
            pinned: pinSuggestion(next),
          });
        }
      })
      .catch((err) => {
        console.error("[useHomeSuggestion] suggestion failed", err);
        if (!stale()) setLoaded({ profileId, day: today, suggestion: null });
      });
    return () => {
      cancelled = true;
    };
  }, [draw.ready, profileId, foreignDraw, today, attempt, pinned, setDraw]);

  const suggestion =
    loaded && loaded.profileId === profileId && loaded.day === today
      ? loaded.suggestion
      : null;
  if (!suggestion) return null;
  const { track } = suggestion;

  const play = async () => {
    setStarting(true);
    try {
      const ids = await startRadio(track.id);
      await playerPlayTracks("radio", null, ids.length ? ids : [track.id], 0);
    } catch (err) {
      console.error("[useHomeSuggestion] play failed", err);
    } finally {
      setStarting(false);
    }
  };

  const another = () => {
    requestRef.current += 1;
    void setDraw({
      profileId,
      day: today,
      attempt: attempt + 1,
      pinned: null,
    });
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

  return { suggestion, track, kindLabel, reason, starting, play, another };
}

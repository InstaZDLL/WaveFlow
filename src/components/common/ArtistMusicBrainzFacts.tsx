import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { ExternalLink } from "lucide-react";

import {
  getArtistMusicBrainz,
  musicBrainzUrl,
  parseGenres,
  type ArtistFacts,
} from "../../lib/tauri/enrichment";
import { openUrl } from "../../lib/tauri/external";

/** The year of a `YYYY[-MM[-DD]]` date, as written. */
function yearOf(date: string | null): string | null {
  return date && /^\d{4}/.test(date) ? date.slice(0, 4) : null;
}

/**
 * What MusicBrainz says about an artist the user accepted in the
 * enrichment review: its kind, where it is from, when it was active,
 * its genres. Drawn only for an accepted artist — a proposal is not a
 * fact until the user says so.
 */
export function ArtistMusicBrainzFacts({ artistId }: { artistId: number }) {
  const { t } = useTranslation();
  const [facts, setFacts] = useState<ArtistFacts | null>(null);

  useEffect(() => {
    let cancelled = false;
    const read = () =>
      getArtistMusicBrainz(artistId)
        .then((found) => {
          if (!cancelled) setFacts(found);
        })
        .catch((err) =>
          console.warn("[ArtistMusicBrainzFacts] read failed", err),
        );
    let off: (() => void) | undefined;
    // Subscribe first, then read: an accept that lands between the two is
    // heard rather than lost (see the "Events" invariant).
    listen<number>("artist:updated", (event) => {
      if (event.payload === artistId) void read();
    })
      .then((unlisten) => {
        if (cancelled) {
          unlisten();
          return;
        }
        off = unlisten;
        void read();
      })
      .catch((err) => {
        console.warn("[ArtistMusicBrainzFacts] listen failed", err);
        if (!cancelled) void read();
      });
    return () => {
      cancelled = true;
      off?.();
    };
  }, [artistId]);

  if (!facts) return null;
  const begin = yearOf(facts.begin_date);
  const end = yearOf(facts.end_date);
  const active =
    begin && end
      ? t("artistDetail.facts.activeRange", { begin, end })
      : begin
        ? t("artistDetail.facts.activeSince", { begin })
        : null;
  const place = facts.area ?? facts.country;
  const line = [facts.kind, place, active].filter(Boolean).join(" · ");
  const genres = parseGenres(facts.genres);
  if (!line && genres.length === 0 && !facts.disambiguation) return null;

  return (
    <div className="space-y-3">
      <h2 className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase px-1">
        {t("artistDetail.facts.title")}
      </h2>
      <div className="rounded-2xl border border-zinc-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-800/40 space-y-2">
        {line && (
          <p className="text-sm text-zinc-700 dark:text-zinc-200">{line}</p>
        )}
        {facts.disambiguation && (
          <p className="text-xs italic text-zinc-500">{facts.disambiguation}</p>
        )}
        {genres.length > 0 && (
          <div className="flex flex-wrap gap-1.5">
            {genres.map((genre) => (
              <span
                key={genre}
                className="px-2 py-0.5 rounded-md bg-zinc-100 dark:bg-zinc-800 text-xs text-zinc-600 dark:text-zinc-300"
              >
                {genre}
              </span>
            ))}
          </div>
        )}
        {facts.artist_mbid && (
          <button
            type="button"
            onClick={() =>
              void openUrl(musicBrainzUrl("artist", facts.artist_mbid!)).catch(
                (err) =>
                  console.warn("[ArtistMusicBrainzFacts] open failed", err),
              )
            }
            className="inline-flex items-center gap-1 text-xs text-zinc-500 hover:text-emerald-600 dark:hover:text-emerald-400"
          >
            <ExternalLink size={12} aria-hidden="true" />
            {t("enrichmentReview.openMusicBrainz")}
          </button>
        )}
      </div>
    </div>
  );
}

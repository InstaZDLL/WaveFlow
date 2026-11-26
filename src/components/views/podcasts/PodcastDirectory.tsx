import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  ChevronLeft,
  Globe,
  Loader2,
  Plus,
  Podcast,
  Search,
} from "lucide-react";

import { usePageVirtualizer } from "../../../hooks/usePageVirtualizer";
import { usePodcastDirectoryCountry } from "../../../hooks/usePodcastDirectoryCountry";
import {
  PODCAST_GENRES,
  PODCAST_STOREFRONTS,
  type DirectoryPlace,
} from "../../../lib/podcastDirectory";
import { podcastFailureMessage } from "../../../lib/podcastWords";
import { countryLabel } from "../../../lib/region";
import { podcastErrorWord } from "../../../lib/tauri/podcast";
import {
  podcastDirectoryArtwork,
  podcastDirectorySearch,
  podcastDirectorySubscribe,
  podcastDirectoryTop,
  type DirectoryShow,
} from "../../../lib/tauri/podcastDirectory";

/** A show's tile, at its narrowest, and the gap between tiles. */
const TILE_MIN_WIDTH = 176;
const TILE_GAP = 24;

/** The pictures already received, by show id, so scrolling back asks the
 *  backend nothing. At most {@link MAX_PICTURES}, the oldest out first. A show
 *  without one is not kept here: the backend knows which absences last, and
 *  asks again for the others. */
const pictures = new Map<number, string>();
const MAX_PICTURES = 400;

function keepPicture(id: number, picture: string) {
  pictures.set(id, picture);
  while (pictures.size > MAX_PICTURES) {
    const oldest = pictures.keys().next();
    if (oldest.done) break;
    pictures.delete(oldest.value);
  }
}

type Row =
  | { kind: "tiles"; key: string; shows: DirectoryShow[]; first: number }
  | { kind: "show"; key: string; show: DirectoryShow; rank: number };

const ESTIMATES: Record<Row["kind"], number> = { tiles: 300, show: 76 };

/**
 * The podcast directory, in the Podcasts page: a search, a country, the genres,
 * and a chart or the results as one virtual list. Shows come with an id and
 * never with a feed's address; subscribing asks by that id, through the same
 * subscription as an address typed.
 *
 * Nothing is asked while offline: the page says so instead. Pictures come
 * from the backend as `data:` addresses, so the webview contacts no host.
 */
export function PodcastDirectory({
  place,
  fluid,
  offline,
  onPlace,
  onOpenChannel,
  onSubscribed,
}: {
  place: DirectoryPlace;
  fluid: boolean;
  offline: boolean;
  /** A new place, on a new history entry. */
  onPlace: (place: DirectoryPlace) => void;
  onOpenChannel: (channelId: number) => void;
  /** A show became a channel: reads the library again, settled once it holds the row. */
  onSubscribed: () => Promise<void>;
}) {
  const { t, i18n } = useTranslation();
  const { country, setCountry, ready } = usePodcastDirectoryCountry();
  const genre = place.term ? null : (place.genre ?? null);
  const term = place.term?.trim() || null;
  const [typed, setTyped] = useState(term ?? "");
  // The field follows Back and Forward: a new term on the entry replaces what
  // was typed, during the render that brings it.
  const [typedFor, setTypedFor] = useState(term);
  if (typedFor !== term) {
    setTypedFor(term);
    setTyped(term ?? "");
  }
  const [attempt, setAttempt] = useState(0);
  // The question the list answers, and what came back. A list whose question
  // is not the current one is still being read.
  const question = `${country}|${genre ?? ""}|${term ?? ""}|${attempt}`;
  const [answer, setAnswer] = useState<{
    question: string;
    shows: DirectoryShow[];
    /** `undefined` when it was read; a word, or `null` for none, when not. */
    failure?: string | null;
  } | null>(null);
  // Subscriptions made here, by show: newer than any list read since.
  const [followed, setFollowed] = useState<Map<number, number>>(new Map());
  const [busy, setBusy] = useState<Set<number>>(new Set());
  const [rowFailure, setRowFailure] = useState<{
    id: number;
    text: string;
  } | null>(null);
  const searchId = useId();
  const countryId = useId();

  useEffect(() => {
    if (!ready || offline) return;
    let current = true;
    const asked = term
      ? podcastDirectorySearch(country, term)
      : podcastDirectoryTop(country, genre);
    asked.then(
      (found) => {
        if (!current) return;
        // `followed` stays: a subscription confirmed while this was read is
        // newer than what this says.
        setAnswer({ question, shows: found });
      },
      (err: unknown) => {
        if (!current) return;
        const word = podcastErrorWord(err);
        if (word == null) console.error("[podcasts] directory read failed");
        setAnswer({ question, shows: [], failure: word });
      },
    );
    return () => {
      current = false;
    };
  }, [ready, offline, country, genre, term, question]);

  const answered = answer?.question === question ? answer : null;
  const loading = answered == null;
  const shows = answered?.shows ?? [];
  const failure =
    answered && answered.failure !== undefined
      ? podcastFailureMessage(t, answered.failure)
      : null;

  const countries = useMemo(
    () =>
      PODCAST_STOREFRONTS.map((code) => ({
        code,
        name: countryLabel(i18n.language, code),
      })).sort((a, b) => a.name.localeCompare(b.name, i18n.language)),
    [i18n.language],
  );
  const countryName = countryLabel(i18n.language, country);

  const subscribe = useCallback(
    async (show: DirectoryShow) => {
      setBusy((ids) => new Set(ids).add(show.id));
      setRowFailure(null);
      try {
        const channel = await podcastDirectorySubscribe(country, show.id);
        // Open is offered once the list holds the channel: a channel it does not
        // hold yet reads as one that is gone.
        await onSubscribed();
        setFollowed((map) => new Map(map).set(show.id, channel.id));
      } catch (err) {
        const word = podcastErrorWord(err);
        if (word == null)
          console.error("[podcasts] directory subscribe failed");
        setRowFailure({ id: show.id, text: podcastFailureMessage(t, word) });
      } finally {
        setBusy((ids) => {
          const next = new Set(ids);
          next.delete(show.id);
          return next;
        });
      }
    },
    [country, onSubscribed, t],
  );

  const heading = term
    ? t("podcasts.directory.resultsFor", { term })
    : genre != null
      ? t("podcasts.directory.genreTop", {
          genre: t(`podcasts.directory.genres.${genre}`),
          country: countryName,
        })
      : t("podcasts.directory.top", { country: countryName });

  const item: ShowItemContext = {
    fluid,
    ranked: !term,
    offline,
    busy,
    followed,
    rowFailure,
    onSubscribe: (show) => void subscribe(show),
    onOpenChannel,
  };

  return (
    <div className="space-y-5" data-podcast-directory>
      <div className="flex flex-wrap items-end gap-3">
        <form
          role="search"
          noValidate
          className="flex min-w-[16rem] flex-1 items-end gap-2"
          onSubmit={(event) => {
            event.preventDefault();
            const next = typed.trim();
            if (next) onPlace({ term: next });
          }}
        >
          <div className="min-w-0 flex-1">
            <label className="pc-label" htmlFor={searchId}>
              {t("podcasts.directory.searchLabel")}
            </label>
            <div className="relative mt-2">
              <Search
                size={15}
                className="pointer-events-none absolute start-3 top-1/2 -translate-y-1/2 opacity-60"
                aria-hidden="true"
              />
              <input
                id={searchId}
                type="search"
                spellCheck={false}
                autoComplete="off"
                maxLength={100}
                value={typed}
                disabled={offline}
                onChange={(event) => setTyped(event.target.value)}
                placeholder={t("podcasts.directory.searchPlaceholder")}
                className="pc-field pc-field--icon w-full"
              />
            </div>
          </div>
          <button
            type="submit"
            className="pc-button"
            data-primary
            disabled={offline || typed.trim() === ""}
          >
            {t("podcasts.directory.searchAction")}
          </button>
        </form>
        <div>
          <label className="pc-label" htmlFor={countryId}>
            {t("podcasts.directory.country")}
          </label>
          <div className="relative mt-2">
            <Globe
              size={15}
              className="pointer-events-none absolute start-3 top-1/2 -translate-y-1/2 opacity-60"
              aria-hidden="true"
            />
            <select
              id={countryId}
              value={country}
              onChange={(event) => setCountry(event.target.value)}
              className="pc-field pc-field--select pc-field--icon max-w-[16rem]"
            >
              {countries.map((entry) => (
                <option key={entry.code} value={entry.code}>
                  {entry.name}
                </option>
              ))}
            </select>
          </div>
        </div>
      </div>
      <p className="pc-meta">{t("podcasts.directory.privacy")}</p>

      {!term && (
        <div
          className="pc-genres"
          role="group"
          aria-label={t("podcasts.directory.genresLabel")}
        >
          {PODCAST_GENRES.map((id) => (
            <button
              key={id}
              type="button"
              className="pc-genre"
              aria-pressed={genre === id}
              onClick={() => onPlace(genre === id ? {} : { genre: id })}
            >
              {t(`podcasts.directory.genres.${id}`)}
            </button>
          ))}
        </div>
      )}

      <div className="pc-heading flex flex-wrap items-baseline gap-x-4 gap-y-1">
        <h2 className="pc-heading__title min-w-0">{heading}</h2>
        {(term || genre != null) && (
          <button
            type="button"
            className="pc-link inline-flex items-center gap-1"
            onClick={() => onPlace({})}
          >
            <ChevronLeft
              size={15}
              className="rtl:rotate-180"
              aria-hidden="true"
            />
            {t("podcasts.directory.backToTop")}
          </button>
        )}
      </div>

      {offline ? (
        <div className="pc-notice" role="status">
          <p className="min-w-0 flex-1">{t("podcasts.directory.offline")}</p>
        </div>
      ) : failure ? (
        <div className="pc-notice" role="alert" data-tone="warn">
          <p className="min-w-0 flex-1">{failure}</p>
          <button
            type="button"
            className="pc-link"
            onClick={() => setAttempt((n) => n + 1)}
          >
            {t("podcasts.retry")}
          </button>
        </div>
      ) : loading ? (
        <p className="pc-meta inline-flex items-center gap-2" role="status">
          <Loader2 size={15} className="animate-spin" aria-hidden="true" />
          {t("podcasts.directory.loading")}
        </p>
      ) : shows.length === 0 ? (
        <p className="pc-meta" role="status">
          {t(
            term
              ? "podcasts.directory.noResults"
              : "podcasts.directory.emptyChart",
          )}
        </p>
      ) : (
        <DirectoryList shows={shows} item={item} />
      )}
    </div>
  );
}

interface ShowItemContext {
  fluid: boolean;
  /** A chart: each show has its place on it. */
  ranked: boolean;
  offline: boolean;
  busy: Set<number>;
  followed: Map<number, number>;
  rowFailure: { id: number; text: string } | null;
  onSubscribe: (show: DirectoryShow) => void;
  onOpenChannel: (channelId: number) => void;
}

/** The shows as one virtual list on the page's scroller: tiles chunked into
 *  rows on the fluid layout, a row each on the classic one. */
function DirectoryList({
  shows,
  item,
}: {
  shows: DirectoryShow[];
  item: ShowItemContext;
}) {
  const [columns, setColumns] = useState(4);
  const rows = useMemo<Row[]>(() => {
    if (!item.fluid) {
      return shows.map((show, index) => ({
        kind: "show",
        key: `s-${show.id}`,
        show,
        rank: index + 1,
      }));
    }
    const out: Row[] = [];
    for (let start = 0; start < shows.length; start += columns) {
      out.push({
        kind: "tiles",
        key: `t-${start}`,
        shows: shows.slice(start, start + columns),
        first: start + 1,
      });
    }
    return out;
  }, [shows, item.fluid, columns]);

  const { parentRef, virtualizer, scrollMargin } = usePageVirtualizer({
    count: rows.length,
    estimateSize: (index) => ESTIMATES[rows[index]?.kind ?? "show"],
    getItemKey: (index) => rows[index]?.key ?? index,
  });

  useLayoutEffect(() => {
    const element = parentRef.current;
    if (!element) return;
    const measure = () => {
      const width = element.getBoundingClientRect().width;
      setColumns(
        Math.max(
          2,
          Math.floor((width + TILE_GAP) / (TILE_MIN_WIDTH + TILE_GAP)),
        ),
      );
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [parentRef]);

  return (
    <div ref={parentRef} data-podcast-directory-list>
      <div
        style={{
          height: `${virtualizer.getTotalSize()}px`,
          position: "relative",
        }}
      >
        {virtualizer.getVirtualItems().map((virtual) => {
          const row = rows[virtual.index];
          if (!row) return null;
          return (
            <div
              key={virtual.key}
              ref={virtualizer.measureElement}
              data-index={virtual.index}
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                transform: `translateY(${virtual.start - scrollMargin}px)`,
              }}
            >
              {row.kind === "tiles" ? (
                <div
                  className="grid pb-6"
                  style={{
                    gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))`,
                    gap: `${TILE_GAP}px`,
                  }}
                >
                  {row.shows.map((show, index) => (
                    <DirectoryTile
                      key={show.id}
                      show={show}
                      rank={row.first + index}
                      item={item}
                    />
                  ))}
                </div>
              ) : (
                <DirectoryRow show={row.show} rank={row.rank} item={item} />
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

/** A show's picture, asked of the backend once it is drawn. */
function DirectoryCover({
  show,
  className = "",
  iconSize = 22,
}: {
  show: DirectoryShow;
  className?: string;
  iconSize?: number;
}) {
  const [src, setSrc] = useState<string | null>(
    () => pictures.get(show.id) ?? null,
  );
  useEffect(() => {
    // A picture already kept was read into the state when the cover was drawn.
    if (!show.has_artwork || pictures.has(show.id)) return;
    // Asked on every run, a second run of the effect included: the backend keeps
    // what it fetched, so asking twice costs nothing, while a run that skipped
    // the request would leave the picture it started undrawn.
    let current = true;
    podcastDirectoryArtwork(show.id).then(
      (picture) => {
        if (picture != null) keepPicture(show.id, picture);
        if (current) setSrc(picture);
      },
      () => {
        // No picture is shown: the show stays usable, and the next drawing asks again.
      },
    );
    return () => {
      current = false;
    };
  }, [show.id, show.has_artwork]);
  return (
    <span className={`pc-cover ${className}`} aria-hidden="true">
      {src ? (
        <img src={src} alt="" />
      ) : (
        <Podcast size={iconSize} strokeWidth={1.5} />
      )}
    </span>
  );
}

/** What a show says about itself under its title. */
function ShowMeta({ show }: { show: DirectoryShow }) {
  const { t } = useTranslation();
  return (
    <span className="pc-meta flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
      {show.genre_id != null && (
        <span className="pc-meta__part">
          {t(`podcasts.directory.genres.${show.genre_id}`)}
        </span>
      )}
      {show.episode_count != null && (
        <span className="pc-meta__part">
          {t("podcasts.episodesCount", { count: show.episode_count })}
        </span>
      )}
      {show.explicit && (
        <span className="pc-chip">{t("podcasts.directory.explicit")}</span>
      )}
    </span>
  );
}

/** Subscribe, or open what it already is. */
function ShowAction({
  show,
  item,
}: {
  show: DirectoryShow;
  item: ShowItemContext;
}) {
  const { t } = useTranslation();
  const channelId = item.followed.get(show.id) ?? show.channel_id;
  if (channelId != null) {
    return (
      <button
        type="button"
        className="pc-button"
        aria-label={t("podcasts.directory.openNamed", { title: show.title })}
        onClick={() => item.onOpenChannel(channelId)}
      >
        <Check size={15} aria-hidden="true" />
        {t("podcasts.directory.open")}
      </button>
    );
  }
  const busy = item.busy.has(show.id);
  return (
    <button
      type="button"
      className="pc-button"
      data-primary
      disabled={busy || item.offline}
      aria-label={t("podcasts.directory.subscribeNamed", { title: show.title })}
      onClick={() => item.onSubscribe(show)}
    >
      {busy ? (
        <Loader2 size={15} className="animate-spin" aria-hidden="true" />
      ) : (
        <Plus size={15} aria-hidden="true" />
      )}
      {t("podcasts.subscribe.action")}
    </button>
  );
}

function RowFailure({
  show,
  item,
}: {
  show: DirectoryShow;
  item: ShowItemContext;
}) {
  if (item.rowFailure?.id !== show.id) return null;
  return (
    <p role="alert" className="pc-error">
      {item.rowFailure.text}
    </p>
  );
}

/** A show as a row, on the classic layout. */
function DirectoryRow({
  show,
  rank,
  item,
}: {
  show: DirectoryShow;
  rank: number;
  item: ShowItemContext;
}) {
  return (
    <div className="pc-dir-row" data-directory-show={show.id}>
      {item.ranked && (
        <span className="pc-rank" aria-hidden="true">
          {rank}
        </span>
      )}
      <DirectoryCover show={show} className="pc-channel-row__art" />
      <span className="min-w-0 flex-1">
        <span className="pc-title block truncate">{show.title}</span>
        {show.author && (
          <span className="pc-meta block truncate">{show.author}</span>
        )}
        <ShowMeta show={show} />
        <RowFailure show={show} item={item} />
      </span>
      <ShowAction show={show} item={item} />
    </div>
  );
}

/** A show as a tile, on the fluid layout. */
function DirectoryTile({
  show,
  rank,
  item,
}: {
  show: DirectoryShow;
  rank: number;
  item: ShowItemContext;
}) {
  return (
    <div className="pc-channel-tile" data-directory-show={show.id}>
      <span className="fe-tile pc-channel-tile__art">
        <DirectoryCover show={show} iconSize={36} />
        {item.ranked && (
          <span className="pc-count pc-count--over" aria-hidden="true">
            {rank}
          </span>
        )}
      </span>
      <span className="pc-title mt-3 block truncate" title={show.title}>
        {show.title}
      </span>
      {show.author && (
        <span className="pc-meta mt-0.5 block truncate">{show.author}</span>
      )}
      <span className="mt-1.5 block">
        <ShowMeta show={show} />
      </span>
      <span className="mt-2 block">
        <ShowAction show={show} item={item} />
      </span>
      <RowFailure show={show} item={item} />
    </div>
  );
}

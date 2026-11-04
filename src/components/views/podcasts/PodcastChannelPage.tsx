import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import {
  ArrowLeft,
  CheckCheck,
  ExternalLink,
  Loader2,
  Pause,
  Play,
  RefreshCw,
  Undo2,
  UserMinus,
} from "lucide-react";

import { usePageVirtualizer } from "../../../hooks/usePageVirtualizer";
import {
  PODCAST_UPDATED,
  podcastErrorWord,
  podcastListEpisodes,
  type PodcastChannel,
  type PodcastDownloadPayload,
  type PodcastEpisode,
  type PodcastUpdatedPayload,
} from "../../../lib/tauri/podcast";
import { podcastFailureMessage } from "../../../lib/podcastWords";
import {
  channelTitle,
  formatDateTime,
  isInProgress,
  isPlayable,
} from "./format";
import {
  PodcastCover,
  PodcastEpisodeRow,
  type EpisodeActions,
  type PodcastPlayerState,
} from "./PodcastItems";
import { PodcastSubscribeForm } from "./PodcastSubscribeForm";

/** How many rows one read asks for. The command answers at most 500. */
const PAGE_SIZE = 200;

/** How close to the last row loaded the next page starts loading. */
const LOAD_AHEAD = 30;

const SPEEDS = [0.75, 1.25, 1.5, 1.75, 2] as const;

/** The channel's episodes, a page at a time. A change re-reads what was loaded,
 *  so the list keeps its length under the scroll. */
function useEpisodePages(channelId: number) {
  const [episodes, setEpisodes] = useState<PodcastEpisode[]>([]);
  const [complete, setComplete] = useState(false);
  const [loading, setLoading] = useState(true);
  const [failedWord, setFailedWord] = useState<string | null | undefined>(
    undefined,
  );
  const seq = useRef(0);
  const countRef = useRef(0);
  const busyRef = useRef(false);

  const reload = useCallback(() => {
    const mine = ++seq.current;
    const wanted = Math.max(countRef.current, PAGE_SIZE);
    busyRef.current = true;
    const read = async () => {
      const rows: PodcastEpisode[] = [];
      while (rows.length < wanted) {
        const limit = Math.min(500, wanted - rows.length);
        const page = await podcastListEpisodes(channelId, limit, rows.length);
        rows.push(...page);
        if (page.length < limit) return { rows, ended: true };
      }
      return { rows, ended: false };
    };
    read()
      .then(({ rows, ended }) => {
        if (mine !== seq.current) return;
        countRef.current = rows.length;
        setEpisodes(rows);
        setComplete(ended);
        setFailedWord(undefined);
      })
      .catch((err) => {
        console.error("[podcasts] listing the episodes failed", err);
        if (mine === seq.current) setFailedWord(podcastErrorWord(err));
      })
      .finally(() => {
        if (mine !== seq.current) return;
        busyRef.current = false;
        setLoading(false);
      });
  }, [channelId]);

  const loadMore = useCallback(() => {
    if (busyRef.current || complete) return;
    const mine = ++seq.current;
    busyRef.current = true;
    podcastListEpisodes(channelId, PAGE_SIZE, countRef.current)
      .then((page) => {
        if (mine !== seq.current) return;
        setEpisodes((previous) => {
          const known = new Set(previous.map((episode) => episode.id));
          const next = [
            ...previous,
            ...page.filter((episode) => !known.has(episode.id)),
          ];
          countRef.current = next.length;
          return next;
        });
        if (page.length < PAGE_SIZE) setComplete(true);
      })
      .catch((err) => {
        console.error("[podcasts] listing the episodes failed", err);
        if (mine === seq.current) setFailedWord(podcastErrorWord(err));
      })
      .finally(() => {
        if (mine === seq.current) busyRef.current = false;
      });
  }, [channelId, complete]);

  useEffect(() => {
    let cancelled = false;
    let off: (() => void) | undefined;
    listen<PodcastUpdatedPayload>(PODCAST_UPDATED, ({ payload }) => {
      if (payload.channel_id == null || payload.channel_id === channelId) {
        reload();
      }
    })
      .then((unlisten) => {
        if (cancelled) unlisten();
        else off = unlisten;
      })
      .catch((err) => console.warn("[podcasts] listen failed", err))
      .finally(() => {
        if (!cancelled) reload();
      });
    return () => {
      cancelled = true;
      seq.current += 1;
      off?.();
    };
  }, [channelId, reload]);

  return { episodes, complete, loading, failedWord, reload, loadMore };
}

interface PodcastChannelPageProps {
  channel: PodcastChannel;
  /** On, whatever offline mode says. */
  enabled: boolean;
  offline: boolean;
  /** A refresh over the network is running, here or for every channel. */
  refreshing: boolean;
  downloads: ReadonlyMap<number, PodcastDownloadPayload>;
  player: PodcastPlayerState;
  actions: EpisodeActions;
  /** Set each time the episodes changed by a hand here. */
  revision: number;
  onBack: () => void;
  onResubscribed: (channelId: number) => void;
  onRefresh: () => void;
  onMarkAll: (played: boolean) => void;
  onOpenSite: () => void;
  onSetSpeed: (speed: number | null) => void;
  onUnsubscribe: () => void;
  onTurnOn: () => void;
}

/** One channel: who it is, what can be done with it, its episodes. */
export function PodcastChannelPage({
  channel,
  enabled,
  offline,
  refreshing,
  downloads,
  player,
  actions,
  revision,
  onBack,
  onResubscribed,
  onRefresh,
  onMarkAll,
  onOpenSite,
  onSetSpeed,
  onUnsubscribe,
  onTurnOn,
}: PodcastChannelPageProps) {
  const { t, i18n } = useTranslation();
  const pages = useEpisodePages(channel.id);
  const { reload } = pages;
  const [descriptionOpen, setDescriptionOpen] = useState(false);
  const network = enabled && !offline;
  const title = channelTitle(t, channel);

  useEffect(() => {
    if (revision > 0) reload();
  }, [revision, reload]);

  // The row that led here is gone with the list it was in, which leaves the focus
  // on the body: a keyboard carries on from the top of this page, not of the app.
  const backRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (document.activeElement === document.body) {
      backRef.current?.focus({ preventScroll: true });
    }
  }, []);

  const { parentRef, virtualizer, scrollMargin } = usePageVirtualizer({
    count: pages.episodes.length,
    estimateSize: () => 112,
    getItemKey: (index) => pages.episodes[index]?.id ?? index,
  });
  const virtualRows = virtualizer.getVirtualItems();
  const lastIndex = virtualRows[virtualRows.length - 1]?.index ?? -1;
  const { loadMore, complete, episodes } = pages;
  useEffect(() => {
    if (!complete && lastIndex >= episodes.length - LOAD_AHEAD) loadMore();
  }, [complete, lastIndex, episodes.length, loadMore]);

  // What the primary action starts: the episode in the player if it is one of
  // these, else the one in progress, else the newest not played yet, among those
  // loaded.
  const next = useMemo(
    () =>
      episodes.find((episode) => episode.id === player.episodeId) ??
      episodes.find((episode) => isInProgress(episode)) ??
      episodes.find(
        (episode) => episode.played_at == null && isPlayable(episode),
      ),
    [episodes, player.episodeId],
  );
  const nextIsCurrent = next != null && next.id === player.episodeId;
  const sounding = nextIsCurrent && player.playing;
  const canPlayNext =
    next != null && (nextIsCurrent || next.downloaded || network);

  const speedValue =
    channel.playback_speed == null ? "" : String(channel.playback_speed);

  return (
    <div className="space-y-6">
      <button
        ref={backRef}
        type="button"
        className="pc-link pc-back"
        onClick={onBack}
      >
        <ArrowLeft size={14} className="rtl:rotate-180" aria-hidden="true" />
        {t("podcasts.channel.back")}
      </button>

      <div data-page-hero className="flex flex-wrap items-start gap-6">
        <PodcastCover
          path={channel.artwork_path}
          iconSize={56}
          className="pc-cover--hero h-44 w-44 shrink-0 rounded-2xl"
        />
        <div className="min-w-0 flex-1">
          <div className="mb-1 text-[10px] font-bold uppercase tracking-widest text-zinc-400">
            {t("podcasts.channel.kicker")}
          </div>
          <h1 className="text-4xl font-bold text-zinc-900 dark:text-white">
            {title}
          </h1>
          {channel.author && (
            <div className="mt-1 text-sm text-zinc-500">{channel.author}</div>
          )}
          <div className="pc-meta mt-1">
            {t("podcasts.episodesCount", { count: channel.episode_count })}
            {" · "}
            {t("podcasts.unplayedCount", { count: channel.unplayed_count })}
            {channel.last_refreshed_at != null && (
              <>
                {" · "}
                {t("podcasts.channel.checked", {
                  when: formatDateTime(
                    channel.last_refreshed_at,
                    i18n.language,
                  ),
                })}
              </>
            )}
          </div>
          {channel.description && (
            <div className="mt-3 max-w-3xl">
              <p
                className={`pc-body whitespace-pre-line ${descriptionOpen ? "" : "line-clamp-3"}`}
              >
                {channel.description}
              </p>
              {channel.description.length > 240 && (
                <button
                  type="button"
                  className="pc-link"
                  aria-expanded={descriptionOpen}
                  onClick={() => setDescriptionOpen((open) => !open)}
                >
                  {t(
                    descriptionOpen ? "podcasts.showLess" : "podcasts.showMore",
                  )}
                </button>
              )}
            </div>
          )}
          <div className="mt-4 flex flex-wrap items-center gap-2">
            {next && (
              <button
                type="button"
                disabled={!canPlayNext}
                onClick={() => actions.play(next)}
                className="inline-flex items-center gap-2 rounded-full bg-emerald-500 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-600 disabled:cursor-not-allowed disabled:opacity-50"
              >
                {sounding ? (
                  <Pause size={14} className="fill-current" strokeWidth={0} />
                ) : (
                  <Play size={14} className="fill-current" strokeWidth={0} />
                )}
                {t(
                  sounding
                    ? "podcasts.toListen.pause"
                    : nextIsCurrent || isInProgress(next)
                      ? "podcasts.toListen.resume"
                      : "podcasts.toListen.listen",
                )}
              </button>
            )}
            {enabled && channel.has_feed_address && (
              <button
                type="button"
                disabled={offline || refreshing}
                onClick={onRefresh}
                className="inline-flex items-center gap-2 rounded-full border border-zinc-200 px-4 py-2 text-sm font-medium text-zinc-700 hover:bg-zinc-100 disabled:cursor-not-allowed disabled:opacity-50 dark:border-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-800"
              >
                {refreshing ? (
                  <Loader2
                    size={14}
                    className="animate-spin"
                    aria-hidden="true"
                  />
                ) : (
                  <RefreshCw size={14} aria-hidden="true" />
                )}
                {t("podcasts.channel.refresh")}
              </button>
            )}
            <button
              type="button"
              disabled={channel.episode_count === 0}
              onClick={() => onMarkAll(channel.unplayed_count > 0)}
              className="inline-flex items-center gap-2 rounded-full border border-zinc-200 px-4 py-2 text-sm font-medium text-zinc-700 hover:bg-zinc-100 disabled:cursor-not-allowed disabled:opacity-50 dark:border-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-800"
            >
              {channel.unplayed_count > 0 ? (
                <CheckCheck size={14} aria-hidden="true" />
              ) : (
                <Undo2 size={14} aria-hidden="true" />
              )}
              {t(
                channel.unplayed_count > 0
                  ? "podcasts.channel.markAllPlayed"
                  : "podcasts.channel.markAllUnplayed",
              )}
            </button>
            {channel.has_site_url && (
              <button
                type="button"
                onClick={onOpenSite}
                className="inline-flex items-center gap-2 rounded-full border border-zinc-200 px-4 py-2 text-sm font-medium text-zinc-700 hover:bg-zinc-100 dark:border-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-800"
              >
                <ExternalLink size={14} aria-hidden="true" />
                {t("podcasts.channel.openSite")}
              </button>
            )}
            <button
              type="button"
              onClick={onUnsubscribe}
              className="inline-flex items-center gap-2 rounded-full border border-zinc-200 px-4 py-2 text-sm font-medium text-zinc-700 hover:bg-zinc-100 dark:border-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-800"
            >
              <UserMinus size={14} aria-hidden="true" />
              {t("podcasts.channel.unsubscribe")}
            </button>
            <label className="pc-speed">
              <span className="pc-meta">{t("podcasts.channel.speed")}</span>
              <select
                className="pc-field pc-field--select"
                value={speedValue}
                onChange={(event) =>
                  onSetSpeed(
                    event.target.value ? Number(event.target.value) : null,
                  )
                }
              >
                <option value="">{t("podcasts.channel.speedNormal")}</option>
                {SPEEDS.map((speed) => (
                  <option key={speed} value={String(speed)}>
                    {new Intl.NumberFormat(i18n.language).format(speed)}×
                  </option>
                ))}
              </select>
            </label>
          </div>
        </div>
      </div>

      {!channel.has_feed_address && (
        <div className="pc-card" data-podcast-restored>
          <h2 className="pc-title">{t("podcasts.restored.title")}</h2>
          <p className="pc-body mt-1">{t("podcasts.restored.body")}</p>
          <p className="pc-body mt-1">{t("podcasts.restored.downloads")}</p>
          {enabled ? (
            <div className="mt-3">
              <PodcastSubscribeForm
                onSubscribed={onResubscribed}
                disabled={offline}
                hint={t("podcasts.restored.hint")}
              />
            </div>
          ) : (
            <button
              type="button"
              className="pc-button mt-3"
              data-primary
              onClick={onTurnOn}
            >
              {t("podcasts.off.action")}
            </button>
          )}
        </div>
      )}

      {channel.has_feed_address && channel.last_error && (
        <p className="pc-notice" role="status" data-tone="warn">
          {t("podcasts.channel.lastFailure", {
            reason: podcastFailureMessage(t, channel.last_error),
          })}
        </p>
      )}

      {pages.failedWord !== undefined ? (
        <div className="pc-notice" role="alert" data-tone="warn">
          <p>{podcastFailureMessage(t, pages.failedWord)}</p>
          <button type="button" className="pc-link" onClick={pages.reload}>
            {t("podcasts.retry")}
          </button>
        </div>
      ) : pages.loading ? (
        <p className="pc-meta">{t("podcasts.loading")}</p>
      ) : episodes.length === 0 ? (
        <p className="pc-meta">{t("podcasts.channel.noEpisodes")}</p>
      ) : null}

      <div ref={parentRef} data-podcast-episodes>
        <div
          style={{
            height: `${virtualizer.getTotalSize()}px`,
            position: "relative",
          }}
        >
          {virtualRows.map((row) => {
            const episode = episodes[row.index];
            if (!episode) return null;
            return (
              <div
                key={row.key}
                ref={virtualizer.measureElement}
                data-index={row.index}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  transform: `translateY(${row.start - scrollMargin}px)`,
                }}
              >
                <PodcastEpisodeRow
                  episode={episode}
                  network={network}
                  download={downloads.get(episode.id)}
                  player={player}
                  actions={actions}
                />
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

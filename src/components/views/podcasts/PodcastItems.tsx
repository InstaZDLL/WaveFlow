import { useState, type CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  Download,
  Loader2,
  Play,
  Podcast,
  RotateCcw,
  Trash2,
  X,
} from "lucide-react";

import { PrecomputedBlur } from "../../common/PrecomputedBlur";
import { resolveRemoteImage } from "../../../lib/tauri/artwork";
import type {
  PodcastChannel,
  PodcastDownloadPayload,
  PodcastEpisode,
} from "../../../lib/tauri/podcast";
import {
  channelTitle,
  episodeTitle,
  formatBytes,
  formatDate,
  formatLength,
  isInProgress,
  isPlayable,
} from "./format";

/** The cached file only: opening the page contacts no host. */
export function PodcastCover({
  path,
  className = "",
  iconSize = 22,
}: {
  path: string | null;
  className?: string;
  iconSize?: number;
}) {
  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  const src = resolveRemoteImage(path, null);
  return (
    <span className={`pc-cover ${className}`} aria-hidden="true">
      {src && src !== failedSrc ? (
        <img
          src={src}
          alt=""
          loading="lazy"
          onError={() => setFailedSrc(src)}
        />
      ) : (
        <Podcast size={iconSize} strokeWidth={1.5} />
      )}
    </span>
  );
}

/** What a channel's row says about itself besides its name. */
function ChannelState({ channel }: { channel: PodcastChannel }) {
  const { t } = useTranslation();
  if (!channel.has_feed_address) {
    return (
      <span className="pc-chip" data-tone="warn">
        {t("podcasts.channel.restoredChip")}
      </span>
    );
  }
  if (channel.last_error) {
    return (
      <span className="pc-chip" data-tone="warn">
        {t("podcasts.channel.failedChip")}
      </span>
    );
  }
  return null;
}

interface ChannelItemProps {
  channel: PodcastChannel;
  onOpen: (channelId: number) => void;
}

/** A channel as a row, on the classic layout. */
export function PodcastChannelRow({ channel, onOpen }: ChannelItemProps) {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      className="pc-channel-row"
      onClick={() => onOpen(channel.id)}
    >
      <PodcastCover
        path={channel.artwork_path}
        className="pc-channel-row__art"
      />
      <span className="min-w-0 flex-1">
        <span className="pc-title block truncate">
          {channelTitle(t, channel)}
        </span>
        {channel.author && (
          <span className="pc-meta block truncate">{channel.author}</span>
        )}
      </span>
      <ChannelState channel={channel} />
      {channel.unplayed_count > 0 && (
        <span className="pc-count">
          {t("podcasts.unplayedCount", { count: channel.unplayed_count })}
        </span>
      )}
    </button>
  );
}

/** A channel as a tile, on the fluid layout: the cover carries the row. */
export function PodcastChannelTile({ channel, onOpen }: ChannelItemProps) {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      className="pc-channel-tile fe-tile-hover"
      onClick={() => onOpen(channel.id)}
    >
      <span className="fe-tile pc-channel-tile__art">
        <PodcastCover path={channel.artwork_path} iconSize={36} />
        {channel.unplayed_count > 0 && (
          <span className="pc-count pc-count--over">
            {t("podcasts.unplayedCount", { count: channel.unplayed_count })}
          </span>
        )}
      </span>
      <span className="pc-title mt-3 block truncate">
        {channelTitle(t, channel)}
      </span>
      {channel.author && (
        <span className="pc-meta mt-0.5 block truncate">{channel.author}</span>
      )}
      <span className="mt-1.5 flex min-h-0 empty:hidden">
        <ChannelState channel={channel} />
      </span>
    </button>
  );
}

export interface EpisodeActions {
  play: (episode: PodcastEpisode) => void;
  togglePlayed: (episode: PodcastEpisode) => void;
  download: (episode: PodcastEpisode) => void;
  cancelDownload: (episode: PodcastEpisode) => void;
  deleteDownload: (episode: PodcastEpisode) => void;
}

interface EpisodeRowProps {
  episode: PodcastEpisode;
  /** Set where episodes of several channels are mixed. */
  channel?: PodcastChannel;
  /** Whether a stream or a download may start: on, and not offline. */
  network: boolean;
  /** Set while this episode's download moves. */
  download?: PodcastDownloadPayload;
  actions: EpisodeActions;
}

/** Why the play button is off, or `null` when it is on. */
function playBlocker(episode: PodcastEpisode, network: boolean): string | null {
  if (episode.downloaded) return null;
  if (!episode.has_enclosure) return "podcasts.episode.noAddress";
  return network ? null : "podcasts.episode.needsNetwork";
}

/** Date, length, what is left. Tabular, in the row's quiet line. */
function useEpisodeMeta(episode: PodcastEpisode, channel?: PodcastChannel) {
  const { t, i18n } = useTranslation();
  const locale = i18n.language;
  const parts: string[] = [];
  if (channel) parts.push(channelTitle(t, channel));
  if (episode.season != null && episode.episode_number != null) {
    parts.push(
      t("podcasts.episode.seasonEpisode", {
        season: episode.season,
        episode: episode.episode_number,
      }),
    );
  } else if (episode.episode_number != null) {
    parts.push(
      t("podcasts.episode.number", { episode: episode.episode_number }),
    );
  }
  if (episode.published_at != null) {
    parts.push(formatDate(episode.published_at, locale));
  }
  if (episode.duration_ms != null) {
    parts.push(
      isInProgress(episode) && episode.duration_ms > episode.position_ms
        ? t("podcasts.episode.remaining", {
            time: formatLength(
              episode.duration_ms - episode.position_ms,
              locale,
            ),
          })
        : formatLength(episode.duration_ms, locale),
    );
  }
  return parts;
}

function progressOf(episode: PodcastEpisode): number | null {
  if (!isInProgress(episode) || !episode.duration_ms) return null;
  return Math.min(100, (episode.position_ms / episode.duration_ms) * 100);
}

export function PodcastEpisodeRow({
  episode,
  channel,
  network,
  download,
  actions,
}: EpisodeRowProps) {
  const { t, i18n } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const title = episodeTitle(t, episode);
  const meta = useEpisodeMeta(episode, channel);
  const blocker = playBlocker(episode, network);
  const played = episode.played_at != null;
  const progress = progressOf(episode);
  const inProgress = isInProgress(episode);
  const playLabel = t(
    played
      ? "podcasts.episode.playAgain"
      : inProgress
        ? "podcasts.episode.resume"
        : "podcasts.episode.play",
    { title },
  );

  return (
    <div
      className="pc-episode"
      data-played={played || undefined}
      data-episode-id={episode.id}
    >
      {channel && (
        <PodcastCover
          path={channel.artwork_path}
          className="pc-episode__art"
          iconSize={18}
        />
      )}
      <button
        type="button"
        className="pc-play"
        disabled={blocker != null || !isPlayable(episode)}
        aria-label={playLabel}
        title={blocker ? t(blocker) : playLabel}
        onClick={() => actions.play(episode)}
      >
        {played ? (
          <RotateCcw size={15} />
        ) : (
          <Play
            size={15}
            className="translate-x-px fill-current"
            strokeWidth={0}
          />
        )}
      </button>
      <div className="min-w-0 flex-1">
        <div className="pc-title truncate" title={title}>
          {title}
        </div>
        <div className="pc-meta mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5">
          {meta.map((part, index) => (
            <span key={index} className="pc-meta__part">
              {part}
            </span>
          ))}
          {episode.downloaded && (
            <span className="pc-chip">{t("podcasts.episode.downloaded")}</span>
          )}
        </div>
        {episode.description && (
          <div className="mt-1">
            {/* Plain text from the backend, drawn as text: nothing from a feed
                reaches the DOM as markup. */}
            <p className={`pc-body ${expanded ? "" : "line-clamp-2"}`}>
              {episode.description}
            </p>
            {episode.description.length > 160 && (
              <button
                type="button"
                className="pc-link"
                aria-expanded={expanded}
                onClick={() => setExpanded((open) => !open)}
              >
                {t(expanded ? "podcasts.showLess" : "podcasts.showMore")}
              </button>
            )}
          </div>
        )}
        {progress != null && (
          <div className="pc-progress" aria-hidden="true">
            <span
              style={{ "--pc-progress": `${progress}%` } as CSSProperties}
            />
          </div>
        )}
      </div>
      <div className="pc-episode__actions">
        <EpisodeDownload
          episode={episode}
          download={download}
          network={network}
          actions={actions}
          locale={i18n.language}
          title={title}
        />
        <button
          type="button"
          className="pc-icon-btn"
          aria-pressed={played}
          aria-label={t(
            played
              ? "podcasts.episode.markUnplayed"
              : "podcasts.episode.markPlayed",
            { title },
          )}
          title={t(
            played
              ? "podcasts.episode.markUnplayed"
              : "podcasts.episode.markPlayed",
            { title },
          )}
          onClick={() => actions.togglePlayed(episode)}
        >
          <Check size={16} />
        </button>
      </div>
    </div>
  );
}

function EpisodeDownload({
  episode,
  download,
  network,
  actions,
  locale,
  title,
}: {
  episode: PodcastEpisode;
  download?: PodcastDownloadPayload;
  network: boolean;
  actions: EpisodeActions;
  locale: string;
  title: string;
}) {
  const { t } = useTranslation();
  if (download) {
    const percent =
      download.totalBytes && download.totalBytes > 0
        ? Math.floor((download.receivedBytes / download.totalBytes) * 100)
        : null;
    const label = t("podcasts.episode.cancelDownload", { title });
    return (
      <>
        <span className="pc-meta tabular-nums" role="status">
          {percent != null ? (
            `${percent} %`
          ) : (
            <Loader2 size={14} className="animate-spin" aria-hidden="true" />
          )}
        </span>
        <button
          type="button"
          className="pc-icon-btn"
          aria-label={label}
          title={label}
          onClick={() => actions.cancelDownload(episode)}
        >
          <X size={16} />
        </button>
      </>
    );
  }
  if (episode.downloaded) {
    const label = t("podcasts.episode.deleteDownload", {
      title,
      size:
        episode.download_size != null
          ? formatBytes(episode.download_size, locale)
          : "",
    });
    return (
      <button
        type="button"
        className="pc-icon-btn"
        aria-label={label}
        title={label}
        onClick={() => actions.deleteDownload(episode)}
      >
        <Trash2 size={16} />
      </button>
    );
  }
  const label = t(
    network && episode.has_enclosure
      ? "podcasts.episode.download"
      : "podcasts.episode.needsNetwork",
    { title },
  );
  return (
    <button
      type="button"
      className="pc-icon-btn"
      disabled={!network || !episode.has_enclosure}
      aria-label={label}
      title={label}
      onClick={() => actions.download(episode)}
    >
      <Download size={16} />
    </button>
  );
}

/**
 * The first episode to listen to, on the fluid layout: the Home's band, its
 * cover lighting its own end of it. The material of each skin paints `fe-suggest`
 * already, and blurs nothing per frame.
 */
export function PodcastLeadBand({
  episode,
  channel,
  network,
  onPlay,
}: {
  episode: PodcastEpisode;
  channel?: PodcastChannel;
  network: boolean;
  onPlay: (episode: PodcastEpisode) => void;
}) {
  const { t } = useTranslation();
  const title = episodeTitle(t, episode);
  const meta = useEpisodeMeta(episode);
  const blocker = playBlocker(episode, network);
  const inProgress = isInProgress(episode);
  const blurSrc = resolveRemoteImage(channel?.artwork_path, null);
  return (
    <section
      aria-label={title}
      data-episode-id={episode.id}
      className="fe-suggest pc-lead relative isolate overflow-hidden rounded-[1.125rem] bg-[var(--fe-surface)]"
    >
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
        <span className="fe-tile h-28 w-28 shrink-0">
          <PodcastCover path={channel?.artwork_path ?? null} iconSize={32} />
        </span>
        <div className="min-w-[16rem] flex-1">
          <p className="fe-editorial fe-ink-2 text-[1.375rem] leading-tight">
            {t(
              inProgress
                ? "podcasts.toListen.inProgress"
                : "podcasts.toListen.newest",
            )}
          </p>
          <h3 className="fe-title fe-ink mt-1.5 truncate text-[1.625rem]">
            {title}
          </h3>
          <p className="fe-ink-2 mt-1 truncate text-[0.9375rem]">
            {channel ? channelTitle(t, channel) : ""}
          </p>
          <p className="fe-ink-3 mt-2 truncate text-[0.8125rem]">
            {meta.join(" · ")}
          </p>
        </div>
        <button
          type="button"
          className="fe-cta shrink-0 disabled:cursor-not-allowed disabled:opacity-60"
          disabled={blocker != null}
          title={blocker ? t(blocker) : undefined}
          onClick={() => onPlay(episode)}
        >
          <span className="fe-cta__icon">
            <Play
              size={14}
              className="translate-x-px fill-current"
              strokeWidth={0}
              aria-hidden="true"
            />
          </span>
          <span>
            {t(
              inProgress
                ? "podcasts.toListen.resume"
                : "podcasts.toListen.listen",
            )}
          </span>
        </button>
      </div>
    </section>
  );
}

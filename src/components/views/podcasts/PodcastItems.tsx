import { useState, type CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  Download,
  Loader2,
  Pause,
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
import { playBlocker, progressOf, useEpisodeMeta } from "./episodeMeta";
import {
  channelTitle,
  episodeTitle,
  formatBytes,
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

/** The episode the player holds, and whether it is sounding. */
export interface PodcastPlayerState {
  episodeId: number | null;
  playing: boolean;
}

export interface EpisodeActions {
  /** Its page, where its chapters are asked for. */
  open: (episode: PodcastEpisode) => void;
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
  player: PodcastPlayerState;
  actions: EpisodeActions;
}

export function PodcastEpisodeRow({
  episode,
  channel,
  network,
  download,
  player,
  actions,
}: EpisodeRowProps) {
  const { t, i18n } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const title = episodeTitle(t, episode);
  const meta = useEpisodeMeta(episode, channel);
  // The episode in the player answers to the player, whatever else is off.
  const current = player.episodeId === episode.id;
  const sounding = current && player.playing;
  const blocker = current ? null : playBlocker(episode, network);
  const played = episode.played_at != null;
  const progress = progressOf(episode);
  const inProgress = isInProgress(episode);
  const playLabel = t(
    sounding
      ? "podcasts.episode.pause"
      : current || inProgress
        ? "podcasts.episode.resume"
        : played
          ? "podcasts.episode.playAgain"
          : "podcasts.episode.play",
    { title },
  );

  return (
    <div
      className="pc-episode"
      data-played={played || undefined}
      data-current={current || undefined}
      aria-current={current || undefined}
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
        disabled={blocker != null || (!current && !isPlayable(episode))}
        aria-label={playLabel}
        title={blocker ? t(blocker) : playLabel}
        onClick={() => actions.play(episode)}
      >
        {sounding ? (
          <Pause size={15} className="fill-current" strokeWidth={0} />
        ) : played && !current ? (
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
        <button
          type="button"
          className="pc-title pc-title--link block max-w-full truncate text-start"
          title={title}
          aria-label={t("podcasts.episode.open", { title })}
          onClick={() => actions.open(episode)}
        >
          {title}
        </button>
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

export function EpisodeDownload({
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
          {download.state === "queued" ? (
            t("podcasts.episode.queued")
          ) : percent != null ? (
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
    !episode.has_enclosure
      ? "podcasts.episode.noAddress"
      : network
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
  player,
  onPlay,
  onOpen,
}: {
  episode: PodcastEpisode;
  channel?: PodcastChannel;
  network: boolean;
  player: PodcastPlayerState;
  onPlay: (episode: PodcastEpisode) => void;
  onOpen: (episode: PodcastEpisode) => void;
}) {
  const { t } = useTranslation();
  const title = episodeTitle(t, episode);
  const meta = useEpisodeMeta(episode);
  const current = player.episodeId === episode.id;
  const sounding = current && player.playing;
  const blocker = current ? null : playBlocker(episode, network);
  const inProgress = current || isInProgress(episode);
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
              sounding
                ? "podcasts.toListen.playing"
                : inProgress
                  ? "podcasts.toListen.inProgress"
                  : "podcasts.toListen.newest",
            )}
          </p>
          <h3 className="fe-title fe-ink mt-1.5 truncate text-[1.625rem]">
            <button
              type="button"
              className="pc-title--link max-w-full truncate text-start"
              aria-label={t("podcasts.episode.open", { title })}
              onClick={() => onOpen(episode)}
            >
              {title}
            </button>
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
            {sounding ? (
              <Pause
                size={14}
                className="fill-current"
                strokeWidth={0}
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
          <span>
            {t(
              sounding
                ? "podcasts.toListen.pause"
                : inProgress
                  ? "podcasts.toListen.resume"
                  : "podcasts.toListen.listen",
            )}
          </span>
        </button>
      </div>
    </section>
  );
}

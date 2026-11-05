import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowLeft,
  CloudDownload,
  Loader2,
  Pause,
  Play,
  Podcast,
  Plus,
  RefreshCw,
  RotateCcw,
  Server,
  Trash2,
} from "lucide-react";

import { usePageTop } from "../../../hooks/usePageTop";
import { usePageVirtualizer } from "../../../hooks/usePageVirtualizer";
import { usePodcastServerChannel } from "../../../hooks/usePodcastServer";
import { useRemoteArtworkSrc } from "../../../hooks/useRemoteArtworkSrc";
import { podcastFailureMessage } from "../../../lib/podcastWords";
import {
  podcastServerSubscribe,
  type ServerChannel,
  type ServerEpisode,
} from "../../../lib/tauri/podcastServer";
import { formatDate, formatDateTime, formatLength } from "./format";
import { PodcastSubscribeForm } from "./PodcastSubscribeForm";

/** What a row of a server's channel can ask for. */
export interface ServerEpisodeActions {
  play: (episode: ServerEpisode) => void;
  forgetPosition: (episode: ServerEpisode) => void;
  requestDownload: (episode: ServerEpisode) => void;
  deleteEpisode: (episode: ServerEpisode) => void;
}

/** The server's episode in the player, and whether it is sounding. */
export interface ServerPlayerState {
  episodeId: string | null;
  playing: boolean;
}

function title(
  t: (key: string) => string,
  value: string,
  fallback: "podcasts.untitledChannel" | "podcasts.untitledEpisode",
) {
  return value.trim() || t(fallback);
}

/** A cover from the server's artwork store, cached on disk the first time. */
function ServerCover({
  hash,
  className = "",
  iconSize = 22,
}: {
  hash: string | null;
  className?: string;
  iconSize?: number;
}) {
  const { src, onError, onLoad } = useRemoteArtworkSrc(hash);
  return (
    <span className={`pc-cover ${className}`} aria-hidden="true">
      {src ? (
        <img src={src} alt="" loading="lazy" onError={onError} onLoad={onLoad} />
      ) : (
        <Podcast size={iconSize} strokeWidth={1.5} />
      )}
    </span>
  );
}

/** Says a channel is the server's, wherever one shows beside a local one. */
export function ServerChip() {
  const { t } = useTranslation();
  return (
    <span className="pc-chip" data-podcast-server-chip>
      <Server size={11} className="me-1 inline-block" aria-hidden="true" />
      {t("podcasts.server.chip")}
    </span>
  );
}

interface ServerChannelItemProps {
  channel: ServerChannel;
  onOpen: (channelId: string) => void;
}

/** A server's channel as a row, on the classic layout. */
export function ServerChannelRow({ channel, onOpen }: ServerChannelItemProps) {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      className="pc-channel-row"
      data-podcast-server-channel={channel.id}
      onClick={() => onOpen(channel.id)}
    >
      <ServerCover hash={channel.artwork_hash} className="pc-channel-row__art" />
      <span className="min-w-0 flex-1">
        <span className="pc-title block truncate">
          {title(t, channel.title, "podcasts.untitledChannel")}
        </span>
        <span className="pc-meta block truncate">
          {t("podcasts.episodesCount", { count: channel.episode_count })}
        </span>
      </span>
      <ServerChip />
    </button>
  );
}

/** A server's channel as a tile, on the fluid layout. */
export function ServerChannelTile({ channel, onOpen }: ServerChannelItemProps) {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      className="pc-channel-tile fe-tile-hover"
      data-podcast-server-channel={channel.id}
      onClick={() => onOpen(channel.id)}
    >
      <span className="fe-tile pc-channel-tile__art">
        <ServerCover hash={channel.artwork_hash} iconSize={36} />
      </span>
      <span className="pc-title mt-3 block truncate">
        {title(t, channel.title, "podcasts.untitledChannel")}
      </span>
      <span className="mt-1.5 flex">
        <ServerChip />
      </span>
    </button>
  );
}

/** The head of the server's section: what it is, and for a manager what can be done. */
export function ServerSectionHead({
  canManage,
  disabled,
  onRefresh,
  onSubscribed,
}: {
  canManage: boolean;
  disabled: boolean;
  onRefresh: () => void;
  onSubscribed: (channelId: string) => void;
}) {
  const { t } = useTranslation();
  const [adding, setAdding] = useState(false);
  return (
    <div className="pc-heading" data-podcast-server-head>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 className="pc-heading__title">{t("podcasts.server.title")}</h2>
        {canManage && (
          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              className="pc-button"
              disabled={disabled}
              onClick={onRefresh}
            >
              <RefreshCw size={15} aria-hidden="true" />
              {t("podcasts.server.refresh")}
            </button>
            <button
              type="button"
              className="pc-button"
              aria-expanded={adding}
              disabled={disabled}
              onClick={() => setAdding((open) => !open)}
            >
              <Plus size={15} aria-hidden="true" />
              {t("podcasts.server.subscribeOpen")}
            </button>
          </div>
        )}
      </div>
      {canManage && adding && (
        <div className="pc-card mt-3">
          <PodcastSubscribeForm
            subscribe={podcastServerSubscribe}
            label={t("podcasts.server.subscribeLabel")}
            hint={t("podcasts.server.subscribeHint")}
            disabled={disabled}
            onSubscribed={onSubscribed}
          />
        </div>
      )}
    </div>
  );
}

/** Where a server's episode stands, when it is anything but ready to play. */
function EpisodeState({ episode }: { episode: ServerEpisode }) {
  const { t } = useTranslation();
  if (episode.status === "completed") return null;
  if (episode.status === "downloading") {
    return (
      <span className="pc-chip" role="status" data-podcast-server-state="downloading">
        <Loader2
          size={11}
          className="me-1 inline-block animate-spin"
          aria-hidden="true"
        />
        {t("podcasts.server.downloading")}
      </span>
    );
  }
  if (episode.status === "error") {
    return (
      <span
        className="pc-chip"
        data-tone="warn"
        data-podcast-server-state="error"
        title={podcastFailureMessage(t, episode.error)}
      >
        {t("podcasts.server.downloadFailed")}
      </span>
    );
  }
  return (
    <span
      className="pc-chip"
      data-podcast-server-state="not-stored"
      title={t("podcasts.server.notStoredHint")}
    >
      {t("podcasts.server.notStored")}
    </span>
  );
}

function ServerEpisodeRow({
  episode,
  canManage,
  network,
  player,
  actions,
}: {
  episode: ServerEpisode;
  canManage: boolean;
  network: boolean;
  player: ServerPlayerState;
  actions: ServerEpisodeActions;
}) {
  const { t, i18n } = useTranslation();
  const locale = i18n.language;
  const [expanded, setExpanded] = useState(false);
  const name = title(t, episode.title, "podcasts.untitledEpisode");
  const stored = episode.status === "completed";
  const current = player.episodeId === episode.id;
  const sounding = current && player.playing;
  const position = episode.position_ms ?? 0;
  const inProgress = position > 0;
  const blocker = current
    ? null
    : !stored
      ? "podcasts.server.notStoredHint"
      : network
        ? null
        : "podcasts.server.needsServer";
  const playLabel = t(
    sounding
      ? "podcasts.episode.pause"
      : current || inProgress
        ? "podcasts.episode.resume"
        : "podcasts.episode.play",
    { title: name },
  );

  const meta: string[] = [];
  if (episode.published_at != null) {
    meta.push(formatDate(episode.published_at, locale));
  }
  if (episode.duration_ms != null) {
    meta.push(
      inProgress && episode.duration_ms > position
        ? t("podcasts.episode.remaining", {
            time: formatLength(episode.duration_ms - position, locale),
          })
        : formatLength(episode.duration_ms, locale),
    );
  }
  const progress =
    inProgress && episode.duration_ms
      ? Math.min(100, (position / episode.duration_ms) * 100)
      : null;

  return (
    <div
      className="pc-episode"
      data-current={current || undefined}
      aria-current={current || undefined}
      data-server-episode-id={episode.id}
    >
      <button
        type="button"
        className="pc-play"
        disabled={blocker != null}
        aria-label={playLabel}
        title={blocker ? t(blocker) : playLabel}
        onClick={() => actions.play(episode)}
      >
        {sounding ? (
          <Pause size={15} className="fill-current" strokeWidth={0} />
        ) : (
          <Play
            size={15}
            className="translate-x-px fill-current"
            strokeWidth={0}
          />
        )}
      </button>
      <div className="min-w-0 flex-1">
        <div className="pc-title truncate" title={name}>
          {name}
        </div>
        <div className="pc-meta mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5">
          {meta.map((part, index) => (
            <span key={index} className="pc-meta__part">
              {part}
            </span>
          ))}
          <EpisodeState episode={episode} />
        </div>
        {episode.description && (
          <div className="mt-1">
            {/* Drawn as text: nothing from a feed reaches the DOM as markup. */}
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
              style={{ "--pc-progress": `${progress}%` } as React.CSSProperties}
            />
          </div>
        )}
      </div>
      <div className="pc-episode__actions">
        {inProgress && (
          <button
            type="button"
            className="pc-icon-btn"
            disabled={!network}
            aria-label={t("podcasts.server.forgetPosition", { title: name })}
            title={t("podcasts.server.forgetPosition", { title: name })}
            onClick={() => actions.forgetPosition(episode)}
          >
            <RotateCcw size={15} aria-hidden="true" />
          </button>
        )}
        {canManage && (episode.status === "new" || episode.status === "error") && (
          <button
            type="button"
            className="pc-icon-btn"
            disabled={!network}
            aria-label={t("podcasts.server.requestDownload", { title: name })}
            title={t("podcasts.server.requestDownload", { title: name })}
            onClick={() => actions.requestDownload(episode)}
          >
            <CloudDownload size={15} aria-hidden="true" />
          </button>
        )}
        {canManage && (
          <button
            type="button"
            className="pc-icon-btn"
            disabled={!network}
            aria-label={t("podcasts.server.deleteEpisode", { title: name })}
            title={t("podcasts.server.deleteEpisode", { title: name })}
            onClick={() => actions.deleteEpisode(episode)}
          >
            <Trash2 size={15} aria-hidden="true" />
          </button>
        )}
      </div>
    </div>
  );
}

/**
 * One channel of the server, read when it opens. No chapters, no transcript, no played
 * state and no speed menu: the server's answer has none of them, and nothing about its
 * channels is kept here. `canManage` comes from the overview, the one place the server
 * says it, and a page it has not arrived on yet shows a reader's controls.
 */
export function ServerChannelPage({
  channelId,
  canManage,
  network,
  player,
  actions,
  revision,
  onBack,
  onGone,
  onRefresh,
  onUnsubscribe,
}: {
  channelId: string;
  canManage: boolean;
  /** On, and not offline. */
  network: boolean;
  player: ServerPlayerState;
  actions: ServerEpisodeActions;
  /** Set each time something was changed by a hand here. */
  revision: number;
  onBack: () => void;
  onGone: () => void;
  onRefresh: () => void;
  onUnsubscribe: (channel: ServerChannel) => void;
}) {
  const { t, i18n } = useTranslation();
  usePageTop();
  const { channel, loading, failedWord, reload } = usePodcastServerChannel(
    channelId,
    revision,
  );
  const [descriptionOpen, setDescriptionOpen] = useState(false);
  const episodes = useMemo(() => channel?.episodes ?? [], [channel]);

  useEffect(() => {
    if (failedWord === "channel_gone") onGone();
  }, [failedWord, onGone]);

  const backRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (document.activeElement === document.body) {
      backRef.current?.focus({ preventScroll: true });
    }
  }, []);

  const { parentRef, virtualizer, scrollMargin } = usePageVirtualizer({
    count: episodes.length,
    estimateSize: () => 112,
    getItemKey: (index) => episodes[index]?.id ?? index,
  });

  // The episode in the player if it is one of these, else one in progress, else the
  // newest the server holds.
  const next = useMemo(() => {
    const stored = episodes.filter((episode) => episode.status === "completed");
    return (
      episodes.find((episode) => episode.id === player.episodeId) ??
      stored.find((episode) => (episode.position_ms ?? 0) > 0) ??
      stored[0]
    );
  }, [episodes, player.episodeId]);
  const nextIsCurrent = next != null && next.id === player.episodeId;
  const sounding = nextIsCurrent && player.playing;
  const name = channel
    ? title(t, channel.title, "podcasts.untitledChannel")
    : "";

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

      {channel && (
        <div data-page-hero className="flex flex-wrap items-start gap-6">
          <ServerCover
            hash={channel.artwork_hash}
            iconSize={56}
            className="pc-cover--hero h-44 w-44 shrink-0 rounded-2xl"
          />
          <div className="min-w-0 flex-1">
            <div className="mb-1 flex items-center gap-2">
              <span className="text-[10px] font-bold uppercase tracking-widest text-zinc-400">
                {t("podcasts.channel.kicker")}
              </span>
              <ServerChip />
            </div>
            <h1 className="text-4xl font-bold text-zinc-900 dark:text-white">
              {name}
            </h1>
            <div className="pc-meta mt-1">
              {t("podcasts.episodesCount", { count: channel.episode_count })}
              {channel.refreshed_at != null && (
                <>
                  {" · "}
                  {t("podcasts.channel.checked", {
                    when: formatDateTime(channel.refreshed_at, i18n.language),
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
                      descriptionOpen
                        ? "podcasts.showLess"
                        : "podcasts.showMore",
                    )}
                  </button>
                )}
              </div>
            )}
            <div className="mt-4 flex flex-wrap items-center gap-2">
              {next && (
                <button
                  type="button"
                  disabled={!nextIsCurrent && !network}
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
                      : nextIsCurrent || (next.position_ms ?? 0) > 0
                        ? "podcasts.toListen.resume"
                        : "podcasts.toListen.listen",
                  )}
                </button>
              )}
              {canManage && (
                <>
                  <button
                    type="button"
                    className="pc-button"
                    disabled={!network}
                    onClick={onRefresh}
                  >
                    <RefreshCw size={14} aria-hidden="true" />
                    {t("podcasts.server.refresh")}
                  </button>
                  <button
                    type="button"
                    className="pc-button"
                    disabled={!network}
                    onClick={() => onUnsubscribe(channel)}
                  >
                    <Trash2 size={14} aria-hidden="true" />
                    {t("podcasts.server.unsubscribe")}
                  </button>
                </>
              )}
            </div>
          </div>
        </div>
      )}

      {channel?.error && (
        <p className="pc-notice" role="status" data-tone="warn">
          {t("podcasts.channel.lastFailure", {
            reason: podcastFailureMessage(t, channel.error),
          })}
        </p>
      )}

      {failedWord !== undefined && failedWord !== "channel_gone" ? (
        <div className="pc-notice" role="alert" data-tone="warn">
          <p>{podcastFailureMessage(t, failedWord)}</p>
          <button type="button" className="pc-link" onClick={reload}>
            {t("podcasts.retry")}
          </button>
        </div>
      ) : loading ? (
        <p className="pc-meta">{t("podcasts.loading")}</p>
      ) : channel && episodes.length === 0 ? (
        <p className="pc-meta">{t("podcasts.channel.noEpisodes")}</p>
      ) : null}

      <div ref={parentRef} data-podcast-episodes>
        <div
          style={{
            height: `${virtualizer.getTotalSize()}px`,
            position: "relative",
          }}
        >
          {virtualizer.getVirtualItems().map((row) => {
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
                <ServerEpisodeRow
                  episode={episode}
                  canManage={canManage}
                  network={network}
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

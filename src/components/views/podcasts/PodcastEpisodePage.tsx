import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import {
  ArrowLeft,
  Check,
  FileText,
  Pause,
  Play,
  RotateCcw,
} from "lucide-react";

import { usePageTop } from "../../../hooks/usePageTop";
import { usePlayer } from "../../../hooks/usePlayer";
import {
  PODCAST_UPDATED,
  podcastErrorWord,
  podcastGetEpisode,
  type PodcastChannel,
  type PodcastDownloadPayload,
  type PodcastEpisode,
  type PodcastUpdatedPayload,
} from "../../../lib/tauri/podcast";
import { podcastFailureMessage } from "../../../lib/podcastWords";
import { PodcastPageChapters } from "../../podcasts/PodcastChapters";
import { channelTitle, episodeTitle, isInProgress } from "./format";
import { playBlocker, progressOf, useEpisodeMeta } from "./episodeMeta";
import {
  EpisodeDownload,
  PodcastCover,
  type EpisodeActions,
  type PodcastPlayerState,
} from "./PodcastItems";

/** The row, read again whenever an episode of its channel may have changed. */
function useEpisodeRow(
  episodeId: number,
  channelId: number,
  revision: number,
  onGone: () => void,
) {
  const [episode, setEpisode] = useState<PodcastEpisode | null>(null);
  const [failedWord, setFailedWord] = useState<string | null | undefined>(
    undefined,
  );
  const seq = useRef(0);
  // A prop that is new at each render of the view must not make this read again.
  const goneRef = useRef(onGone);
  useEffect(() => {
    goneRef.current = onGone;
  }, [onGone]);
  const reload = useCallback(() => {
    const mine = ++seq.current;
    podcastGetEpisode(episodeId)
      .then((row) => {
        if (mine !== seq.current) return;
        // An entry kept across a profile switch names another profile's rows: the
        // same id may well be an episode here, of some other channel.
        if (row.channel_id !== channelId) {
          goneRef.current();
          return;
        }
        setEpisode(row);
        setFailedWord(undefined);
      })
      .catch((err) => {
        if (mine !== seq.current) return;
        const word = podcastErrorWord(err);
        if (word === "episode_gone") {
          goneRef.current();
          return;
        }
        if (word == null) console.error("[podcasts] episode read failed", err);
        setFailedWord(word);
      });
  }, [episodeId, channelId]);

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

  useEffect(() => {
    if (revision > 0) reload();
  }, [revision, reload]);

  return { episode, failedWord, reload };
}

interface PodcastEpisodePageProps {
  episodeId: number;
  channel: PodcastChannel;
  /** Whether a stream or a download may start: on, and not offline. */
  network: boolean;
  download?: PodcastDownloadPayload;
  player: PodcastPlayerState;
  actions: EpisodeActions;
  revision: number;
  onBack: () => void;
  onGone: () => void;
}

/**
 * One episode: what it says of itself, and its chapters. Opening it is what asks
 * for them, so the channel's list and a refresh contact nobody on their behalf.
 */
export function PodcastEpisodePage({
  episodeId,
  channel,
  network,
  download,
  player,
  actions,
  revision,
  onBack,
  onGone,
}: PodcastEpisodePageProps) {
  const { t, i18n } = useTranslation();
  usePageTop();
  const { openTranscript } = usePlayer();
  const { episode, failedWord, reload } = useEpisodeRow(
    episodeId,
    channel.id,
    revision,
    onGone,
  );

  // The row that led here is gone with the list it was in, which leaves the focus
  // on the body: a keyboard carries on from the top of this page, not of the app.
  const backRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (document.activeElement === document.body) {
      backRef.current?.focus({ preventScroll: true });
    }
  }, []);

  const back = (
    <button
      ref={backRef}
      type="button"
      className="pc-link pc-back"
      onClick={onBack}
    >
      <ArrowLeft size={14} className="rtl:rotate-180" aria-hidden="true" />
      {channelTitle(t, channel)}
    </button>
  );

  if (!episode) {
    return (
      <div className="space-y-6">
        {back}
        {failedWord !== undefined ? (
          <div className="pc-notice" role="alert" data-tone="warn">
            <p className="min-w-0 flex-1">
              {podcastFailureMessage(t, failedWord)}
            </p>
            <button type="button" className="pc-link" onClick={reload}>
              {t("podcasts.retry")}
            </button>
          </div>
        ) : (
          <p className="pc-meta">{t("podcasts.loading")}</p>
        )}
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {back}
      <div data-page-hero className="flex flex-wrap items-start gap-6">
        <PodcastCover
          path={channel.artwork_path}
          iconSize={56}
          className="pc-cover--hero h-44 w-44 shrink-0 rounded-2xl"
        />
        <EpisodeHero
          episode={episode}
          network={network}
          download={download}
          player={player}
          actions={actions}
          locale={i18n.language}
          onTranscript={() => openTranscript(episode.id)}
        />
      </div>
      <PodcastPageChapters key={episode.id} episodeId={episode.id} />
    </div>
  );
}

function EpisodeHero({
  episode,
  network,
  download,
  player,
  actions,
  locale,
  onTranscript,
}: {
  episode: PodcastEpisode;
  network: boolean;
  download?: PodcastDownloadPayload;
  player: PodcastPlayerState;
  actions: EpisodeActions;
  locale: string;
  onTranscript: () => void;
}) {
  const { t } = useTranslation();
  const title = episodeTitle(t, episode);
  const meta = useEpisodeMeta(episode);
  const current = player.episodeId === episode.id;
  const sounding = current && player.playing;
  const blocker = current ? null : playBlocker(episode, network);
  const played = episode.played_at != null;
  const progress = progressOf(episode);
  const playLabel = t(
    sounding
      ? "podcasts.toListen.pause"
      : current || isInProgress(episode)
        ? "podcasts.toListen.resume"
        : played
          ? "podcasts.episode.playAgainShort"
          : "podcasts.toListen.listen",
  );
  const markLabel = t(
    played ? "podcasts.episode.markUnplayed" : "podcasts.episode.markPlayed",
    { title },
  );
  return (
    <div className="min-w-0 flex-1">
      <div className="mb-1 text-[10px] font-bold uppercase tracking-widest text-zinc-400">
        {t("podcasts.episodePage.kicker")}
      </div>
      <h1
        className="text-3xl font-bold text-zinc-900 dark:text-white"
        dir="auto"
      >
        {title}
      </h1>
      <div className="pc-meta mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5">
        {meta.map((part, index) => (
          <span key={index} className="pc-meta__part">
            {part}
          </span>
        ))}
        {episode.downloaded && (
          <span className="pc-chip">{t("podcasts.episode.downloaded")}</span>
        )}
      </div>
      {progress != null && (
        <div className="pc-progress" aria-hidden="true">
          <span style={{ "--pc-progress": `${progress}%` } as CSSProperties} />
        </div>
      )}
      <div className="mt-4 flex flex-wrap items-center gap-2">
        <button
          type="button"
          disabled={blocker != null}
          title={blocker ? t(blocker) : undefined}
          onClick={() => actions.play(episode)}
          className="inline-flex items-center gap-2 rounded-full bg-emerald-500 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-600 disabled:cursor-not-allowed disabled:opacity-50"
        >
          {sounding ? (
            <Pause size={14} className="fill-current" strokeWidth={0} />
          ) : played && !current ? (
            <RotateCcw size={14} aria-hidden="true" />
          ) : (
            <Play size={14} className="fill-current" strokeWidth={0} />
          )}
          {playLabel}
        </button>
        {episode.has_transcript && (
          <button
            type="button"
            onClick={onTranscript}
            className="inline-flex items-center gap-2 rounded-full border border-zinc-200 px-4 py-2 text-sm font-medium text-zinc-700 hover:bg-zinc-100 dark:border-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-800"
          >
            <FileText size={14} aria-hidden="true" />
            {t("podcasts.transcript.title")}
          </button>
        )}
        <EpisodeDownload
          episode={episode}
          download={download}
          network={network}
          actions={actions}
          locale={locale}
          title={title}
        />
        <button
          type="button"
          className="pc-icon-btn"
          aria-pressed={played}
          aria-label={markLabel}
          title={markLabel}
          onClick={() => actions.togglePlayed(episode)}
        >
          <Check size={16} />
        </button>
      </div>
      {episode.description && (
        // Plain text from the backend, drawn as text.
        <p className="pc-body mt-4 max-w-3xl whitespace-pre-line" dir="auto">
          {episode.description}
        </p>
      )}
    </div>
  );
}

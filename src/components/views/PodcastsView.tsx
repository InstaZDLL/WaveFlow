import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import {
  open as showOpenDialog,
  save as showSaveDialog,
} from "@tauri-apps/plugin-dialog";
import {
  Download,
  Loader2,
  Podcast,
  Power,
  RefreshCw,
  Settings as SettingsIcon,
  Upload,
  WifiOff,
  X,
} from "lucide-react";

import { useSkin } from "../../hooks/useSkin";
import { usePageVirtualizer } from "../../hooks/usePageVirtualizer";
import { usePlayer } from "../../hooks/usePlayer";
import { usePodcastDownloads } from "../../hooks/usePodcastDownloads";
import { usePodcastLibrary } from "../../hooks/usePodcastLibrary";
import { usePodcastServerOverview } from "../../hooks/usePodcastServer";
import { usePodcastsEnabled } from "../../hooks/usePodcastsEnabled";
import { useTasks } from "../../hooks/useTasks";
import {
  importPodcastOpml,
  openPodcastWarning,
  openView,
} from "../../lib/discovery";
import {
  episodeInPlayer as episodeHeldBy,
  serverEpisodeInPlayer as serverEpisodeHeldBy,
} from "../../lib/playerSources";
import { podcastFailureMessage } from "../../lib/podcastWords";
import { getOfflineMode } from "../../lib/tauri/offline";
import {
  podcastCancelDownload,
  podcastDeleteDownload,
  podcastDownloadEpisode,
  podcastErrorWord,
  podcastExportOpml,
  podcastMarkChannelPlayed,
  podcastMarkPlayed,
  podcastOpenSite,
  podcastPlayEpisode,
  podcastRefresh,
  podcastSetChannelSpeed,
  podcastSubscribe,
  podcastUnsubscribe,
  type PodcastChannel,
  type PodcastEpisode,
  type PodcastRefreshSummary,
} from "../../lib/tauri/podcast";
import {
  podcastServerClearPosition,
  podcastServerDeleteEpisode,
  podcastServerPlay,
  podcastServerRefresh,
  podcastServerRequestDownload,
  podcastServerUnsubscribe,
  type ServerChannel,
  type ServerEpisode,
  type ServerOverview,
} from "../../lib/tauri/podcastServer";
import { EmptyState } from "../common/EmptyState";
import { PodcastDialogShell } from "../podcasts/PodcastDialogShell";
import { channelTitle, formatBytes } from "./podcasts/format";
import { PodcastChannelPage } from "./podcasts/PodcastChannelPage";
import { PodcastEpisodePage } from "./podcasts/PodcastEpisodePage";
import {
  PodcastChannelRow,
  PodcastChannelTile,
  PodcastEpisodeRow,
  PodcastLeadBand,
  type EpisodeActions,
  type PodcastPlayerState,
} from "./podcasts/PodcastItems";
import {
  ServerChannelPage,
  ServerChannelRow,
  ServerChannelTile,
  ServerSectionHead,
  type ServerEpisodeActions,
  type ServerPlayerState,
} from "./podcasts/PodcastServer";
import { PodcastSubscribeForm } from "./podcasts/PodcastSubscribeForm";

interface PodcastsViewProps {
  /** The channel on the history entry; `null` is the overview. */
  channelId: number | null;
  /** The episode on the history entry; `null` is its channel, or the overview. */
  episodeId: number | null;
  /** The bound server's channel on the history entry, by its id over there. */
  serverChannelId: string | null;
  onOpenChannel: (channelId: number | null) => void;
  onOpenEpisode: (channelId: number, episodeId: number) => void;
  onOpenServerChannel: (channelId: string) => void;
  /** That channel is gone: replaces the entry, so Back has no ghost to reach. */
  onChannelGone: () => void;
  /** That episode is gone: the same, to its channel. */
  onEpisodeGone: (channelId: number) => void;
}

/** How many of "To listen" show until asked for the rest. */
const TO_LISTEN_SHOWN = 6;

/** A channel's tile, at its narrowest, and the gap between tiles. */
const TILE_MIN_WIDTH = 176;
const TILE_GAP = 24;

const NOTICE_MS = 7_000;

/** One line of the overview's one list. */
type OverviewRow =
  | { kind: "heading"; key: string; title: string; note?: string }
  | { kind: "lead"; key: string; episode: PodcastEpisode }
  | { kind: "episode"; key: string; episode: PodcastEpisode }
  | { kind: "more"; key: string; hidden: number }
  | { kind: "tiles"; key: string; channels: PodcastChannel[] }
  | { kind: "channel"; key: string; channel: PodcastChannel }
  | { kind: "server-head"; key: string }
  | { kind: "server-tiles"; key: string; channels: ServerChannel[] }
  | { kind: "server-channel"; key: string; channel: ServerChannel };

const ESTIMATES: Record<OverviewRow["kind"], number> = {
  heading: 72,
  lead: 180,
  episode: 104,
  more: 44,
  tiles: 280,
  channel: 80,
  "server-head": 72,
  "server-tiles": 280,
  "server-channel": 80,
};

/** The server checks its feeds in the background and does not say when it is done, so
 *  the page reads again once, after long enough for most of them. */
const SERVER_REFRESH_READ_MS = 5_000;

/**
 * Podcasts (RFC-009): what to listen to, the channels followed, one channel's
 * episodes. The rows are drawn from two tables and contact nobody, so the page
 * reads with the switch off too; what would make a request is what goes away.
 *
 * One view for every skin: `skin.layout` chooses the shapes (a band and tiles on
 * the fluid layout, rows on the classic one), and `podcasts.css` paints them from
 * the tokens of whichever material is on, never from a skin's id.
 *
 * A bound server's channels are a section of their own, read over the network when
 * the page opens and absent whenever that cannot be done (RFC-009 decision 11).
 */
export function PodcastsView({
  channelId,
  episodeId,
  serverChannelId,
  onOpenChannel,
  onOpenEpisode,
  onOpenServerChannel,
  onChannelGone,
  onEpisodeGone,
}: PodcastsViewProps) {
  const { t, i18n } = useTranslation();
  const { skin } = useSkin();
  const fluid = skin.layout === "fluid";
  const { enabled, ready } = usePodcastsEnabled();
  const library = usePodcastLibrary();
  const { reload } = library;
  const tasks = useTasks();
  const refreshing = tasks.some((task) => task.kind === "podcast_refresh");
  const [offline, setOffline] = useState(false);
  const [notice, setNotice] = useState<{ text: string; warn: boolean } | null>(
    null,
  );
  const [revision, setRevision] = useState(0);
  const [exportOpen, setExportOpen] = useState(false);
  const [leaving, setLeaving] = useState<PodcastChannel | null>(null);
  const [leavingBusy, setLeavingBusy] = useState(false);
  const [leavingServer, setLeavingServer] = useState<ServerChannel | null>(
    null,
  );
  const [deletingEpisode, setDeletingEpisode] = useState<ServerEpisode | null>(
    null,
  );

  useEffect(() => {
    getOfflineMode()
      .then(setOffline)
      .catch((err) => console.warn("[podcasts] offline read failed", err));
  }, []);

  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), NOTICE_MS);
    return () => window.clearTimeout(timer);
  }, [notice]);

  const failWith = useCallback(
    (word: string | null) => {
      if (word === "offline") setOffline(true);
      setNotice({ text: podcastFailureMessage(t, word), warn: true });
    },
    [t],
  );
  const onDownloadSettled = useCallback(() => {
    void reload();
    setRevision((value) => value + 1);
  }, [reload]);
  const downloads = usePodcastDownloads(failWith, onDownloadSettled);

  const { currentTrack, isPlaying, playbackState, togglePlayback } =
    usePlayer();
  const episodeInPlayer = episodeHeldBy(currentTrack, playbackState);
  const player = useMemo<PodcastPlayerState>(
    () => ({ episodeId: episodeInPlayer, playing: isPlaying }),
    [episodeInPlayer, isPlaying],
  );
  const serverEpisodeInPlayer = serverEpisodeHeldBy(
    currentTrack,
    playbackState,
  );
  const serverPlayer = useMemo<ServerPlayerState>(
    () => ({ episodeId: serverEpisodeInPlayer, playing: isPlaying }),
    [serverEpisodeInPlayer, isPlaying],
  );

  /** Runs a command, says what went wrong, and re-reads what it may have moved. */
  const run = useCallback(
    async (label: string, command: () => Promise<unknown>) => {
      let succeeded = true;
      try {
        await command();
      } catch (err) {
        succeeded = false;
        const word = podcastErrorWord(err);
        if (word == null) console.error(`[podcasts] ${label} failed`, err);
        failWith(word);
      }
      void reload();
      setRevision((value) => value + 1);
      return succeeded;
    },
    [failWith, reload],
  );

  const actions = useMemo<EpisodeActions>(
    () => ({
      open: (episode) => onOpenEpisode(episode.channel_id, episode.id),
      // The episode already in the player pauses or carries on, from where it is
      // rather than from the last position written.
      play: (episode) =>
        void run("play", () =>
          episode.id === episodeInPlayer
            ? togglePlayback()
            : podcastPlayEpisode(episode.id),
        ),
      togglePlayed: (episode) =>
        void run("mark", () =>
          podcastMarkPlayed(episode.id, episode.played_at == null),
        ),
      download: (episode) =>
        void run("download", () => podcastDownloadEpisode(episode.id)),
      cancelDownload: (episode) =>
        void run("cancel", () => podcastCancelDownload(episode.id)),
      // What plays keeps its open file, so the sound does not stop. Said out loud,
      // since the next time it will need the network.
      deleteDownload: (episode) =>
        void run("delete", () => podcastDeleteDownload(episode.id)).then(
          (deleted) => {
            if (deleted && episode.id === episodeInPlayer) {
              setNotice({
                text: t("podcasts.episode.deletedWhilePlaying"),
                warn: false,
              });
            }
          },
        ),
    }),
    [run, episodeInPlayer, togglePlayback, onOpenEpisode, t],
  );

  const network = enabled && !offline;
  const { overview: serverOverview, reload: reloadServer } =
    usePodcastServerOverview(ready && network, serverChannelId);

  const serverActions = useMemo<ServerEpisodeActions>(
    () => ({
      play: (episode) =>
        void run("server play", () =>
          episode.id === serverEpisodeInPlayer
            ? togglePlayback()
            : podcastServerPlay(episode.channel_id, episode.id),
        ),
      forgetPosition: (episode) =>
        void run("server position", () =>
          podcastServerClearPosition(episode.id),
        ),
      requestDownload: (episode) =>
        void run("server download", () =>
          podcastServerRequestDownload(episode.id),
        ),
      deleteEpisode: setDeletingEpisode,
    }),
    [run, serverEpisodeInPlayer, togglePlayback],
  );

  const refreshServer = useCallback(() => {
    void run("server refresh", async () => {
      await podcastServerRefresh();
      setNotice({ text: t("podcasts.server.refreshAsked"), warn: false });
      window.setTimeout(() => {
        void reloadServer();
        setRevision((value) => value + 1);
      }, SERVER_REFRESH_READ_MS);
    });
  }, [run, t, reloadServer]);

  const confirmLeavingServer = async () => {
    if (!leavingServer) return;
    const id = leavingServer.id;
    setLeavingBusy(true);
    const left = await run("server unsubscribe", () =>
      podcastServerUnsubscribe(id),
    );
    setLeavingBusy(false);
    setLeavingServer(null);
    void reloadServer();
    if (left && serverChannelId === id) onChannelGone();
  };

  const confirmDeletingEpisode = async () => {
    if (!deletingEpisode) return;
    const id = deletingEpisode.id;
    setLeavingBusy(true);
    await run("server delete", () => podcastServerDeleteEpisode(id));
    setLeavingBusy(false);
    setDeletingEpisode(null);
  };

  const reportRefresh = useCallback(
    (summary: PodcastRefreshSummary) => {
      if (summary.outcome === "done") {
        setNotice({
          text: t("podcasts.refresh.done", {
            refreshed: summary.refreshed,
            unchanged: summary.unchanged,
            failed: summary.failed,
          }),
          warn: summary.failed > 0,
        });
      } else {
        setNotice({
          text: t(`podcasts.refresh.${summary.outcome}`),
          warn: false,
        });
      }
    },
    [t],
  );

  const refresh = useCallback(
    (id?: number) =>
      void run("refresh", async () => {
        reportRefresh(await podcastRefresh(id));
      }),
    [run, reportRefresh],
  );

  const importList = async () => {
    const picked = await showOpenDialog({
      multiple: false,
      directory: false,
      title: t("podcasts.opml.importDialogTitle"),
      filters: [{ name: "OPML", extensions: ["opml", "xml"] }],
    });
    if (typeof picked === "string") importPodcastOpml(picked);
  };

  const exportList = async () => {
    setExportOpen(false);
    const path = await showSaveDialog({
      title: t("podcasts.opml.exportDialogTitle"),
      defaultPath: "waveflow-podcasts.opml",
      filters: [{ name: "OPML", extensions: ["opml"] }],
    });
    if (!path) return;
    await run("export", async () => {
      const report = await podcastExportOpml(path);
      setNotice({
        text: t("podcasts.opml.exported", {
          written: report.written,
          missing: report.without_address,
        }),
        warn: false,
      });
    });
  };

  const confirmLeaving = async () => {
    if (!leaving) return;
    const id = leaving.id;
    setLeavingBusy(true);
    const left = await run("unsubscribe", () => podcastUnsubscribe(id));
    setLeavingBusy(false);
    setLeaving(null);
    if (left && channelId === id) onChannelGone();
  };

  const turnOn = () => openPodcastWarning();
  const openChannel = useCallback(
    (id: number) => onOpenChannel(id),
    [onOpenChannel],
  );
  // The row has to be in the list first: a channel the list does not hold reads
  // as one that is gone.
  const openSubscribed = useCallback(
    async (id: number) => {
      await reload();
      onOpenChannel(id);
    },
    [reload, onOpenChannel],
  );
  const channel =
    channelId == null
      ? null
      : (library.channels.find((row) => row.id === channelId) ?? null);

  // Unsubscribed from elsewhere, or a Back to a channel since forgotten.
  const gone =
    channelId != null && !library.loading && !library.failed && !channel;
  useEffect(() => {
    if (gone) onChannelGone();
  }, [gone, onChannelGone]);

  const withoutAddress = library.channels.filter(
    (row) => !row.has_feed_address,
  ).length;
  const unplayed = library.channels.reduce(
    (sum, row) => sum + row.unplayed_count,
    0,
  );

  const banners = (
    <>
      {ready && !enabled && (
        <div className="pc-notice" data-podcast-off role="status">
          <Power size={16} className="shrink-0" aria-hidden="true" />
          <p className="min-w-0 flex-1">{t("podcasts.off.body")}</p>
          <button
            type="button"
            className="pc-button"
            data-primary
            onClick={turnOn}
          >
            {t("podcasts.off.action")}
          </button>
        </div>
      )}
      {enabled && offline && (
        <div className="pc-notice" data-podcast-offline role="status">
          <WifiOff size={16} className="shrink-0" aria-hidden="true" />
          <p className="min-w-0 flex-1">{t("podcasts.offlineBanner")}</p>
        </div>
      )}
    </>
  );

  const dialogs = (
    <>
      <PodcastDialogShell
        isOpen={exportOpen}
        mark="opml-export"
        title={t("podcasts.opml.exportTitle")}
        onClose={() => setExportOpen(false)}
        confirm={{
          label: t("podcasts.opml.exportConfirm"),
          busy: false,
          onConfirm: () => void exportList(),
        }}
      >
        <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-400">
          {t("podcasts.opml.exportBody")}
        </p>
        {withoutAddress > 0 && (
          <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-400">
            {t("podcasts.opml.exportMissing", { missing: withoutAddress })}
          </p>
        )}
      </PodcastDialogShell>
      <PodcastDialogShell
        isOpen={leaving != null}
        mark="unsubscribe"
        title={t("podcasts.unsubscribe.title", {
          title: leaving ? channelTitle(t, leaving) : "",
        })}
        onClose={() => setLeaving(null)}
        confirm={{
          label: t("podcasts.unsubscribe.confirm"),
          danger: true,
          busy: leavingBusy,
          onConfirm: () => void confirmLeaving(),
        }}
      >
        <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-400">
          {t("podcasts.unsubscribe.body")}
        </p>
      </PodcastDialogShell>
      <PodcastDialogShell
        isOpen={leavingServer != null}
        mark="server-unsubscribe"
        title={t("podcasts.server.unsubscribeTitle", {
          title: leavingServer?.title.trim() || t("podcasts.untitledChannel"),
        })}
        onClose={() => setLeavingServer(null)}
        confirm={{
          label: t("podcasts.server.unsubscribeConfirm"),
          danger: true,
          busy: leavingBusy,
          onConfirm: () => void confirmLeavingServer(),
        }}
      >
        <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-400">
          {t("podcasts.server.unsubscribeBody")}
        </p>
      </PodcastDialogShell>
      <PodcastDialogShell
        isOpen={deletingEpisode != null}
        mark="server-delete-episode"
        title={t("podcasts.server.deleteEpisodeTitle")}
        onClose={() => setDeletingEpisode(null)}
        confirm={{
          label: t("podcasts.server.unsubscribeConfirm"),
          danger: true,
          busy: leavingBusy,
          onConfirm: () => void confirmDeletingEpisode(),
        }}
      >
        <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-400">
          {t("podcasts.server.deleteEpisodeBody", {
            title: deletingEpisode?.title.trim() || t("podcasts.untitledEpisode"),
          })}
        </p>
      </PodcastDialogShell>
      {notice &&
        createPortal(
          <div
            data-podcasts
            data-podcast-toast
            data-tone={notice.warn ? "warn" : undefined}
            role={notice.warn ? "alert" : "status"}
            className="pc-toast"
          >
            <p className="min-w-0 flex-1">{notice.text}</p>
            <button
              type="button"
              className="pc-icon-btn"
              aria-label={t("podcasts.dialog.close")}
              onClick={() => setNotice(null)}
            >
              <X size={15} aria-hidden="true" />
            </button>
          </div>,
          document.body,
        )}
    </>
  );

  if (serverChannelId != null) {
    return (
      <div
        data-podcasts
        data-podcast-page="server-channel"
        className="mx-auto max-w-[1440px] animate-fade-in space-y-6 pb-20"
      >
        {banners}
        <ServerChannelPage
          key={serverChannelId}
          channelId={serverChannelId}
          canManage={serverOverview?.can_manage ?? false}
          network={network}
          player={serverPlayer}
          actions={serverActions}
          revision={revision}
          onBack={() => onOpenChannel(null)}
          onGone={onChannelGone}
          onRefresh={refreshServer}
          onUnsubscribe={setLeavingServer}
        />
        {dialogs}
      </div>
    );
  }

  if (channel && episodeId != null) {
    return (
      <div
        data-podcasts
        data-podcast-page="episode"
        className="mx-auto max-w-[1440px] animate-fade-in space-y-6 pb-20"
      >
        {banners}
        <PodcastEpisodePage
          key={episodeId}
          episodeId={episodeId}
          channel={channel}
          network={network}
          download={downloads.get(episodeId)}
          player={player}
          actions={actions}
          revision={revision}
          onBack={() => onOpenChannel(channel.id)}
          onGone={() => onEpisodeGone(channel.id)}
        />
        {dialogs}
      </div>
    );
  }

  if (channel) {
    return (
      <div
        data-podcasts
        data-podcast-page="channel"
        className="mx-auto max-w-[1440px] animate-fade-in space-y-6 pb-20"
      >
        {banners}
        <PodcastChannelPage
          key={channel.id}
          channel={channel}
          enabled={enabled}
          offline={offline}
          refreshing={refreshing}
          downloads={downloads}
          player={player}
          actions={actions}
          revision={revision}
          onBack={() => onOpenChannel(null)}
          onResubscribed={() => {
            void reload();
            setRevision((value) => value + 1);
          }}
          onRefresh={() => refresh(channel.id)}
          onMarkAll={(played) =>
            void run("mark all", () =>
              podcastMarkChannelPlayed(channel.id, played),
            )
          }
          onOpenSite={() =>
            void run("open site", () => podcastOpenSite(channel.id))
          }
          onSetSpeed={(speed) =>
            void run("speed", () => podcastSetChannelSpeed(channel.id, speed))
          }
          onUnsubscribe={() => setLeaving(channel)}
          onTurnOn={turnOn}
        />
        {dialogs}
      </div>
    );
  }

  return (
    <div
      data-podcasts
      data-podcast-page="overview"
      className="mx-auto max-w-[1440px] animate-fade-in space-y-6 pb-20"
    >
      <div
        data-page-hero
        className="flex flex-wrap items-start justify-between gap-4"
      >
        <div className="flex min-w-0 items-center space-x-6">
          <div className="flex h-24 w-24 shrink-0 items-center justify-center rounded-2xl bg-violet-100 text-violet-600 shadow-sm dark:bg-violet-950/60 dark:text-violet-400">
            <Podcast size={48} />
          </div>
          <div className="min-w-0">
            <div className="mb-1 text-[10px] font-bold uppercase tracking-widest text-zinc-400">
              {t("podcasts.kicker")}
            </div>
            <h1 className="text-4xl font-bold text-zinc-900 dark:text-white">
              {t("settings.podcasts.title")}
            </h1>
            <div className="mt-1 text-sm text-zinc-500">
              {t("podcasts.channelsCount", {
                count:
                  library.channels.length +
                  (serverOverview?.channels.length ?? 0),
              })}
              {" · "}
              {t("podcasts.unplayedCount", { count: unplayed })}
              {library.usage && library.usage.usedBytes > 0 && (
                <>
                  {" · "}
                  {t("podcasts.usage", {
                    used: formatBytes(library.usage.usedBytes, i18n.language),
                    quota: formatBytes(library.usage.quotaBytes, i18n.language),
                  })}
                </>
              )}
            </div>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {enabled && library.channels.length > withoutAddress && (
            <button
              type="button"
              disabled={offline || refreshing}
              onClick={() => refresh()}
              className="inline-flex items-center gap-2 rounded-full bg-emerald-500 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-600 disabled:cursor-not-allowed disabled:opacity-50"
            >
              {refreshing ? (
                <Loader2
                  size={15}
                  className="animate-spin"
                  aria-hidden="true"
                />
              ) : (
                <RefreshCw size={15} aria-hidden="true" />
              )}
              {t(refreshing ? "podcasts.refreshing" : "podcasts.refreshAll")}
            </button>
          )}
          {enabled && (
            <button
              type="button"
              disabled={offline || refreshing}
              onClick={() => void importList()}
              className="inline-flex items-center gap-2 rounded-full border border-zinc-200 px-4 py-2 text-sm font-medium text-zinc-700 hover:bg-zinc-100 disabled:cursor-not-allowed disabled:opacity-50 dark:border-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-800"
            >
              <Download size={15} aria-hidden="true" />
              {t("podcasts.opml.import")}
            </button>
          )}
          <button
            type="button"
            disabled={library.channels.length === 0}
            onClick={() => setExportOpen(true)}
            className="inline-flex items-center gap-2 rounded-full border border-zinc-200 px-4 py-2 text-sm font-medium text-zinc-700 hover:bg-zinc-100 disabled:cursor-not-allowed disabled:opacity-50 dark:border-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-800"
          >
            <Upload size={15} aria-hidden="true" />
            {t("podcasts.opml.export")}
          </button>
          <button
            type="button"
            className="pc-icon-btn"
            aria-label={t("podcasts.openSettings")}
            title={t("podcasts.openSettings")}
            onClick={() =>
              openView({
                view: "settings",
                settingsCategory: "library",
                settingsGroup: "podcasts",
              })
            }
          >
            <SettingsIcon size={17} aria-hidden="true" />
          </button>
        </div>
      </div>

      {banners}

      {enabled && (
        <div className="pc-card">
          <PodcastSubscribeForm
            subscribe={podcastSubscribe}
            onSubscribed={(id) => void openSubscribed(id)}
            disabled={offline}
          />
          <p className="pc-meta mt-1">{t("podcasts.opml.publicOnly")}</p>
        </div>
      )}

      {withoutAddress > 0 && (
        <div
          className="pc-notice"
          data-podcast-restored
          role="status"
          data-tone="warn"
        >
          <p className="min-w-0 flex-1">
            {t("podcasts.restored.summary", { n: withoutAddress })}
          </p>
        </div>
      )}

      {library.failed ? (
        <div className="pc-notice" role="alert" data-tone="warn">
          <p className="min-w-0 flex-1">
            {podcastFailureMessage(t, library.failedWord)}
          </p>
          <button
            type="button"
            className="pc-link"
            onClick={() => void reload()}
          >
            {t("podcasts.retry")}
          </button>
        </div>
      ) : library.loading || !ready ? (
        <p className="pc-meta">{t("podcasts.loading")}</p>
      ) : library.channels.length === 0 && !serverOverview?.channels.length ? (
        <EmptyState
          icon={<Podcast size={36} strokeWidth={1.5} />}
          title={t("podcasts.empty.title")}
          description={t(
            enabled ? "podcasts.empty.body" : "podcasts.empty.bodyOff",
          )}
        />
      ) : null}

      <PodcastOverviewList
        channels={library.channels}
        toListen={library.toListen}
        toListenFailed={library.toListenFailed}
        fluid={fluid}
        network={network}
        downloads={downloads}
        player={player}
        actions={actions}
        onOpenChannel={openChannel}
        server={serverOverview}
        onOpenServerChannel={onOpenServerChannel}
        serverHead={
          <ServerSectionHead
            canManage={serverOverview?.can_manage ?? false}
            disabled={!network}
            onRefresh={refreshServer}
            onSubscribed={onOpenServerChannel}
          />
        }
      />
      {dialogs}
    </div>
  );
}

/**
 * "To listen", then the channels, as one virtual list: rows of different heights
 * under one measure, driven by the page's scroller. Tiles are chunked into rows
 * from the list's width, as a grid of covers is everywhere else.
 */
function PodcastOverviewList({
  channels,
  toListen,
  toListenFailed,
  fluid,
  network,
  downloads,
  player,
  actions,
  onOpenChannel,
  server,
  onOpenServerChannel,
  serverHead,
}: {
  channels: PodcastChannel[];
  toListen: PodcastEpisode[];
  toListenFailed: boolean;
  fluid: boolean;
  network: boolean;
  downloads: ReturnType<typeof usePodcastDownloads>;
  player: PodcastPlayerState;
  actions: EpisodeActions;
  onOpenChannel: (channelId: number) => void;
  /** `null` is no section: see `usePodcastServerOverview`. */
  server: ServerOverview | null;
  onOpenServerChannel: (channelId: string) => void;
  serverHead: ReactNode;
}) {
  const { t } = useTranslation();
  const [allShown, setAllShown] = useState(false);
  const [columns, setColumns] = useState(4);
  const byId = useMemo(
    () => new Map(channels.map((channel) => [channel.id, channel])),
    [channels],
  );

  const rows = useMemo<OverviewRow[]>(() => {
    const out: OverviewRow[] = [];
    if (channels.length > 0 && (toListen.length > 0 || toListenFailed)) {
      out.push({
        kind: "heading",
        key: "h-listen",
        title: t("podcasts.toListen.title"),
        note: toListenFailed ? t("podcasts.toListen.failed") : undefined,
      });
    }
    const shown = allShown ? toListen : toListen.slice(0, TO_LISTEN_SHOWN);
    shown.forEach((episode, index) => {
      out.push({
        kind: fluid && index === 0 ? "lead" : "episode",
        key: `e-${episode.id}`,
        episode,
      });
    });
    if (shown.length < toListen.length) {
      out.push({
        kind: "more",
        key: "more",
        hidden: toListen.length - shown.length,
      });
    }
    if (channels.length > 0) {
      out.push({
        kind: "heading",
        key: "h-channels",
        title: t("podcasts.channels"),
      });
    }
    if (fluid) {
      for (let start = 0; start < channels.length; start += columns) {
        out.push({
          kind: "tiles",
          key: `t-${start}`,
          channels: channels.slice(start, start + columns),
        });
      }
    } else {
      for (const channel of channels) {
        out.push({ kind: "channel", key: `c-${channel.id}`, channel });
      }
    }
    // A reader with nothing to read has no reason to hear about it. A manager has
    // something to do there.
    if (server && (server.channels.length > 0 || server.can_manage)) {
      out.push({ kind: "server-head", key: "h-server" });
      if (fluid) {
        for (let start = 0; start < server.channels.length; start += columns) {
          out.push({
            kind: "server-tiles",
            key: `st-${start}`,
            channels: server.channels.slice(start, start + columns),
          });
        }
      } else {
        for (const channel of server.channels) {
          out.push({
            kind: "server-channel",
            key: `sc-${channel.id}`,
            channel,
          });
        }
      }
    }
    return out;
  }, [channels, toListen, toListenFailed, allShown, fluid, columns, t, server]);

  const { parentRef, virtualizer, scrollMargin } = usePageVirtualizer({
    count: rows.length,
    estimateSize: (index) => ESTIMATES[rows[index]?.kind ?? "channel"],
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
    <div ref={parentRef} data-podcast-list>
      <div
        style={{
          height: `${virtualizer.getTotalSize()}px`,
          position: "relative",
        }}
      >
        {virtualizer.getVirtualItems().map((item) => {
          const row = rows[item.index];
          if (!row) return null;
          return (
            <div
              key={item.key}
              ref={virtualizer.measureElement}
              data-index={item.index}
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                transform: `translateY(${item.start - scrollMargin}px)`,
              }}
            >
              {row.kind === "heading" && (
                <div className="pc-heading">
                  <h2 className="pc-heading__title">{row.title}</h2>
                  {row.note && <p className="pc-meta mt-1">{row.note}</p>}
                </div>
              )}
              {row.kind === "lead" && (
                <div className="pb-2">
                  <PodcastLeadBand
                    episode={row.episode}
                    channel={byId.get(row.episode.channel_id)}
                    network={network}
                    player={player}
                    onPlay={actions.play}
                    onOpen={actions.open}
                  />
                </div>
              )}
              {row.kind === "episode" && (
                <PodcastEpisodeRow
                  episode={row.episode}
                  channel={byId.get(row.episode.channel_id)}
                  network={network}
                  download={downloads.get(row.episode.id)}
                  player={player}
                  actions={actions}
                />
              )}
              {row.kind === "more" && (
                <button
                  type="button"
                  className="pc-link pc-more"
                  onClick={() => setAllShown(true)}
                >
                  {t("podcasts.toListen.showAll", { n: row.hidden })}
                </button>
              )}
              {row.kind === "tiles" && (
                <div
                  className="grid pb-6"
                  style={{
                    gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))`,
                    gap: `${TILE_GAP}px`,
                  }}
                >
                  {row.channels.map((channel) => (
                    <PodcastChannelTile
                      key={channel.id}
                      channel={channel}
                      onOpen={onOpenChannel}
                    />
                  ))}
                </div>
              )}
              {row.kind === "channel" && (
                <PodcastChannelRow
                  channel={row.channel}
                  onOpen={onOpenChannel}
                />
              )}
              {row.kind === "server-head" && serverHead}
              {row.kind === "server-tiles" && (
                <div
                  className="grid pb-6"
                  style={{
                    gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))`,
                    gap: `${TILE_GAP}px`,
                  }}
                >
                  {row.channels.map((channel) => (
                    <ServerChannelTile
                      key={channel.id}
                      channel={channel}
                      onOpen={onOpenServerChannel}
                    />
                  ))}
                </div>
              )}
              {row.kind === "server-channel" && (
                <ServerChannelRow
                  channel={row.channel}
                  onOpen={onOpenServerChannel}
                />
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

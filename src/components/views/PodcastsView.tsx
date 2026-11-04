import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
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
import { usePodcastDownloads } from "../../hooks/usePodcastDownloads";
import { usePodcastLibrary } from "../../hooks/usePodcastLibrary";
import { usePodcastsEnabled } from "../../hooks/usePodcastsEnabled";
import { useTasks } from "../../hooks/useTasks";
import {
  importPodcastOpml,
  openPodcastWarning,
  openView,
} from "../../lib/discovery";
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
  podcastUnsubscribe,
  type PodcastChannel,
  type PodcastEpisode,
  type PodcastRefreshSummary,
} from "../../lib/tauri/podcast";
import { EmptyState } from "../common/EmptyState";
import { PodcastDialogShell } from "../podcasts/PodcastDialogShell";
import { channelTitle, formatBytes } from "./podcasts/format";
import { PodcastChannelPage } from "./podcasts/PodcastChannelPage";
import {
  PodcastChannelRow,
  PodcastChannelTile,
  PodcastEpisodeRow,
  PodcastLeadBand,
  type EpisodeActions,
} from "./podcasts/PodcastItems";
import { PodcastSubscribeForm } from "./podcasts/PodcastSubscribeForm";

interface PodcastsViewProps {
  /** The channel on the history entry; `null` is the overview. */
  channelId: number | null;
  onOpenChannel: (channelId: number | null) => void;
  /** That channel is gone: replaces the entry, so Back has no ghost to reach. */
  onChannelGone: () => void;
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
  | { kind: "channel"; key: string; channel: PodcastChannel };

const ESTIMATES: Record<OverviewRow["kind"], number> = {
  heading: 72,
  lead: 180,
  episode: 104,
  more: 44,
  tiles: 280,
  channel: 80,
};

/**
 * Podcasts (RFC-009): what to listen to, the channels followed, one channel's
 * episodes. The rows are drawn from two tables and contact nobody, so the page
 * reads with the switch off too; what would make a request is what goes away.
 *
 * One view for every skin: `skin.layout` chooses the shapes (a band and tiles on
 * the fluid layout, rows on the classic one), and `podcasts.css` paints them from
 * the tokens of whichever material is on, never from a skin's id.
 */
export function PodcastsView({
  channelId,
  onOpenChannel,
  onChannelGone,
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
  const downloads = usePodcastDownloads(failWith);

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
      play: (episode) => void run("play", () => podcastPlayEpisode(episode.id)),
      togglePlayed: (episode) =>
        void run("mark", () =>
          podcastMarkPlayed(episode.id, episode.played_at == null),
        ),
      download: (episode) =>
        void run("download", () => podcastDownloadEpisode(episode.id)),
      cancelDownload: (episode) =>
        void run("cancel", () => podcastCancelDownload(episode.id)),
      deleteDownload: (episode) =>
        void run("delete", () => podcastDeleteDownload(episode.id)),
    }),
    [run],
  );

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
  const network = enabled && !offline;
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
              {t("podcasts.channelsCount", { count: library.channels.length })}
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
      ) : library.channels.length === 0 ? (
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
        actions={actions}
        onOpenChannel={openChannel}
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
  actions,
  onOpenChannel,
}: {
  channels: PodcastChannel[];
  toListen: PodcastEpisode[];
  toListenFailed: boolean;
  fluid: boolean;
  network: boolean;
  downloads: ReturnType<typeof usePodcastDownloads>;
  actions: EpisodeActions;
  onOpenChannel: (channelId: number) => void;
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
    if (channels.length === 0) return out;
    if (toListen.length > 0 || toListenFailed) {
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
    out.push({
      kind: "heading",
      key: "h-channels",
      title: t("podcasts.channels"),
    });
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
    return out;
  }, [channels, toListen, toListenFailed, allShown, fluid, columns, t]);

  const { parentRef, virtualizer, scrollMargin } = usePageVirtualizer({
    count: rows.length,
    estimateSize: (index) => ESTIMATES[rows[index]?.kind ?? "channel"],
    getItemKey: (index) => rows[index]?.key ?? index,
  });

  const measuredRef = useRef<HTMLElement | null>(null);
  useLayoutEffect(() => {
    measuredRef.current = parentRef.current;
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
                    onPlay={actions.play}
                  />
                </div>
              )}
              {row.kind === "episode" && (
                <PodcastEpisodeRow
                  episode={row.episode}
                  channel={byId.get(row.episode.channel_id)}
                  network={network}
                  download={downloads.get(row.episode.id)}
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
            </div>
          );
        })}
      </div>
    </div>
  );
}

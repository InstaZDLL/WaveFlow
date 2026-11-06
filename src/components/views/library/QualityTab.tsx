import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
} from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  AudioWaveform,
  CircleCheck,
  Clock,
  FileAudio,
  HelpCircle,
  Loader2,
} from "lucide-react";
import { usePageScroll } from "../../../hooks/usePageScroll";
import { useTasks } from "../../../hooks/useTasks";
import {
  QUALITY_VERDICTS,
  libraryQualitySummary,
  libraryQualityTracks,
  reanalyzeLossless,
  type QualitySummary,
  type QualityTrackRow,
  type QualityVerdict,
} from "../../../lib/tauri/quality";
import { getTrack, type Track } from "../../../lib/tauri/track";
import { Artwork } from "../../common/Artwork";
import { TrackPropertiesModal } from "../../common/TrackPropertiesModal";
import type { Translator } from "./libraryTypes";

/** Rows asked for at a time; the table asks for the next page as the
 *  reader nears the end of what it holds. */
const PAGE = 200;
const ROW_HEIGHT = 56;
/** How long "Analyse again" waits for the second click. */
const CONFIRM_MS = 4000;
/** How often the counts are asked again while the analysis runs. */
const REFRESH_MS = 3000;

/**
 * The formats a reader filters by, as the search chips name them:
 * `ALAC` is told from AAC by its bit depth, and `OGG` is Vorbis
 * (`formats_condition`).
 */
const FORMATS = [
  "FLAC",
  "ALAC",
  "WAV",
  "AIFF",
  "APE",
  "WavPack",
  "MP3",
  "AAC",
  "OGG",
  "Opus",
  "DSF",
  "DFF",
];

const ICONS: Record<QualityVerdict, typeof AudioWaveform> = {
  lossy_source: AudioWaveform,
  pending: Clock,
  consistent: CircleCheck,
  lossy: FileAudio,
  not_measured: HelpCircle,
};

/** The pill's colours: the probable fake is the one finding; the rest
 *  are facts. */
const TONES: Record<QualityVerdict, string> = {
  lossy_source:
    "border-amber-500/60 text-amber-700 dark:text-amber-300 bg-amber-50 dark:bg-amber-950/30",
  pending: "border-zinc-300 text-zinc-500 dark:border-zinc-700",
  consistent:
    "border-emerald-500/50 text-emerald-700 dark:text-emerald-300 bg-emerald-50/60 dark:bg-emerald-950/20",
  lossy:
    "border-zinc-300 text-zinc-600 dark:border-zinc-700 dark:text-zinc-300",
  not_measured: "border-zinc-300 text-zinc-500 dark:border-zinc-700",
};

const GRID = "2.5rem minmax(0,2fr) minmax(0,1.5fr) 8.5rem minmax(0,1.3fr)";

interface QualityTabProps {
  t: Translator;
  locale: string;
  /** Moves when the library changes underneath: a rescan, a tag edit. */
  refreshKey: string;
  /** The counts, for the page header. */
  onSummary: (summary: QualitySummary) => void;
  /** The track's context menu, as every other list opens it. */
  onContextMenu: (event: ReactMouseEvent, track: Track) => void;
}

/**
 * The library's Quality tab: what the audio of each file says about it,
 * measured rather than read from the container (`commands/quality.rs`).
 *
 * Its own table rather than the library's: the three columns that matter
 * here — what the file says, where the highs stop, what the audio says —
 * are not track columns anyone would want on the Tracks tab, and the
 * verdict is never shown in the other lists, where a dull recording taken
 * for an MP3 would carry the accusation on every row. A row opens the
 * track's properties, where the spectrogram shows the cut.
 */
export function QualityTab({
  t,
  locale,
  refreshKey,
  onSummary,
  onContextMenu,
}: QualityTabProps) {
  const [summary, setSummary] = useState<QualitySummary | null>(null);
  const [verdict, setVerdict] = useState<QualityVerdict | null>(null);
  const [format, setFormat] = useState<string>("");
  const [rows, setRows] = useState<QualityTrackRow[]>([]);
  const [total, setTotal] = useState(0);
  const [listLoading, setListLoading] = useState(true);
  // A list that could not be read is said so, not shown as an empty one.
  const [listFailed, setListFailed] = useState(false);
  const [opened, setOpened] = useState<Track | null>(null);
  const tasks = useTasks();
  const analysis = tasks.find((task) => task.kind === "analysis") ?? null;
  const running = analysis != null;

  // Every answer is for the filter it was asked under; one for a filter
  // the reader has left counts as absent.
  const request = useRef(0);
  const loadingMore = useRef(false);

  const loadSummary = useCallback(() => {
    libraryQualitySummary()
      .then((next) => {
        setSummary(next);
        onSummary(next);
      })
      .catch((err) => {
        console.error("[QualityTab] libraryQualitySummary failed", err);
      });
  }, [onSummary]);

  /** The first page of the current filter, replacing what is shown. */
  const loadFirst = useCallback(() => {
    const id = ++request.current;
    loadingMore.current = false;
    setListLoading(true);
    libraryQualityTracks(verdict, format || null, 0, PAGE)
      .then((page) => {
        if (id !== request.current) return;
        setRows(page.items);
        setTotal(page.total);
        setListFailed(false);
      })
      .catch((err) => {
        if (id !== request.current) return;
        console.error("[QualityTab] libraryQualityTracks failed", err);
        setRows([]);
        setTotal(0);
        setListFailed(true);
      })
      .finally(() => {
        if (id === request.current) setListLoading(false);
      });
  }, [verdict, format]);

  // The header and the list are asked together, on every arrival and every
  // change of filter, so the counts above never contradict the rows below.
  useEffect(() => {
    loadSummary();
    loadFirst();
  }, [loadSummary, loadFirst, refreshKey]);

  // While the analysis runs, the counts follow it; when it ends, the list
  // is asked again so the rows it measured move to their verdict.
  useEffect(() => {
    if (!running) return;
    const timer = window.setInterval(loadSummary, REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [running, loadSummary]);
  const wasRunning = useRef(running);
  useEffect(() => {
    if (wasRunning.current && !running) {
      loadSummary();
      loadFirst();
    }
    wasRunning.current = running;
  }, [running, loadSummary, loadFirst]);

  const loadMore = useCallback(() => {
    // Not while the first page of a filter is on its way: the rows on
    // screen are the previous filter's, and their count is not an offset
    // into the new one.
    if (listLoading || loadingMore.current || rows.length >= total) return;
    loadingMore.current = true;
    const id = request.current;
    libraryQualityTracks(verdict, format || null, rows.length, PAGE)
      .then((page) => {
        if (id !== request.current) return;
        // While the analysis runs, a file can move between two pages and
        // arrive twice; the first place it was shown is kept.
        setRows((prev) => {
          const seen = new Set(prev.map((row) => row.id));
          return prev.concat(page.items.filter((row) => !seen.has(row.id)));
        });
        setTotal(page.total);
      })
      .catch((err) => {
        if (id === request.current)
          console.error("[QualityTab] next page failed", err);
      })
      .finally(() => {
        if (id === request.current) loadingMore.current = false;
      });
  }, [verdict, format, rows.length, total, listLoading]);

  const pageScrollRef = usePageScroll();
  const parentRef = useRef<HTMLDivElement>(null);
  const [scrollMargin, setScrollMargin] = useState(0);
  useLayoutEffect(() => {
    const parent = parentRef.current;
    const scroller = pageScrollRef?.current;
    if (!parent || !scroller) return;
    const recompute = () => {
      const parentRect = parent.getBoundingClientRect();
      const scrollerRect = scroller.getBoundingClientRect();
      setScrollMargin(parentRect.top - scrollerRect.top + scroller.scrollTop);
    };
    recompute();
    const ro = new ResizeObserver(recompute);
    ro.observe(parent);
    ro.observe(scroller);
    return () => ro.disconnect();
  }, [pageScrollRef, rows.length]);

  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => pageScrollRef?.current ?? null,
    estimateSize: () => ROW_HEIGHT,
    overscan: 15,
    scrollMargin,
  });
  const items = virtualizer.getVirtualItems();
  const lastIndex = items.length > 0 ? items[items.length - 1].index : -1;
  useEffect(() => {
    if (lastIndex >= rows.length - 40) loadMore();
  }, [lastIndex, rows.length, loadMore]);

  const number = useMemo(() => new Intl.NumberFormat(locale), [locale]);
  const kilo = useMemo(
    () => new Intl.NumberFormat(locale, { maximumFractionDigits: 1 }),
    [locale],
  );
  const when = useMemo(
    () =>
      new Intl.DateTimeFormat(locale, {
        dateStyle: "medium",
        timeStyle: "short",
      }),
    [locale],
  );

  const count = (key: QualityVerdict) =>
    summary?.verdicts.find((entry) => entry.verdict === key)?.count ?? 0;

  const facts = (row: QualityTrackRow) =>
    [
      row.codec,
      row.bit_depth != null && row.bit_depth > 1
        ? t("library.quality.bits", { value: row.bit_depth })
        : null,
      row.sample_rate != null
        ? t("library.quality.khz", {
            value: kilo.format(row.sample_rate / 1000),
          })
        : null,
      row.bitrate != null
        ? t("library.quality.kbps", { value: number.format(row.bitrate) })
        : null,
    ]
      .filter((fact): fact is string => fact != null && fact !== "")
      .join(" · ");

  /** Where the highs stop, in words: a measured wall, the full band of a
   *  file measured without one, or nothing known. */
  const highs = (row: QualityTrackRow) => {
    if (row.verdict === "pending" || row.verdict === "not_measured") return "—";
    if (row.cutoff_hz != null)
      return t("library.quality.khz", {
        value: kilo.format(row.cutoff_hz / 1000),
      });
    return row.verdict === "consistent" ? t("library.quality.fullBand") : "—";
  };

  const withTrack = (id: number, then: (track: Track) => void) => {
    getTrack(id)
      .then((track) => {
        if (track) then(track);
      })
      .catch((err) => {
        console.error("[QualityTab] getTrack failed", err);
      });
  };

  const status = running
    ? analysis.total > 0
      ? t("library.quality.status.running", {
          current: number.format(analysis.current),
          total: number.format(analysis.total),
        })
      : t("library.quality.status.starting")
    : summary?.last_analyzed_at != null
      ? t("library.quality.status.idle", {
          when: when.format(summary.last_analyzed_at),
        })
      : t("library.quality.status.never");

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div className="min-w-0 max-w-3xl space-y-2">
          <p className="text-sm font-medium text-zinc-900 dark:text-white">
            {summary == null
              ? " "
              : summary.lossless === 0
                ? t("library.quality.noLossless")
                : t("library.quality.progress", {
                    measured: number.format(summary.measured),
                    lossless: number.format(summary.lossless),
                  })}
          </p>
          <p
            role="status"
            aria-live="polite"
            className="flex items-center gap-2 text-xs text-zinc-500 dark:text-zinc-400"
          >
            {running && (
              <Loader2
                size={12}
                className="motion-safe:animate-spin text-emerald-500"
                aria-hidden="true"
              />
            )}
            {status}
          </p>
          <p className="text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">
            {t("library.quality.help")}
          </p>
        </div>
        <AnalyseAgain
          t={t}
          disabled={running || summary == null || summary.lossless === 0}
          onDone={() => {
            loadSummary();
            loadFirst();
          }}
        />
      </div>

      {/* The same chips as "Needs attention", so the skins dress both
          alike. A count at zero stays, greyed: the shape of the list does
          not change between two visits. */}
      <div
        role="group"
        aria-label={t("library.quality.filters")}
        className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3"
      >
        <button
          type="button"
          aria-pressed={verdict === null}
          onClick={() => setVerdict(null)}
          className={chipClass(verdict === null, false)}
        >
          <span className="text-sm font-medium text-zinc-900 dark:text-white">
            {t("library.quality.all")}
          </span>
        </button>
        {QUALITY_VERDICTS.map((key) => {
          const Icon = ICONS[key];
          const n = count(key);
          const active = verdict === key;
          return (
            <button
              key={key}
              type="button"
              aria-pressed={active}
              disabled={n === 0 && !active}
              onClick={() => setVerdict(active ? null : key)}
              className={chipClass(active, n === 0 && !active)}
            >
              <Icon
                size={18}
                aria-hidden="true"
                className={`mt-0.5 shrink-0 ${
                  n > 0 && key === "lossy_source"
                    ? "text-amber-500"
                    : "text-zinc-400"
                }`}
              />
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-medium text-zinc-900 dark:text-white">
                  {t(`library.quality.verdicts.${key}`)}
                </span>
              </span>
              <span className="text-xs font-semibold tabular-nums text-zinc-700 dark:text-zinc-200">
                {number.format(n)}
              </span>
            </button>
          );
        })}
      </div>

      <label className="flex items-center gap-3 text-sm text-zinc-700 dark:text-zinc-300">
        <span>{t("library.quality.format")}</span>
        <select
          value={format}
          onChange={(event) => setFormat(event.target.value)}
          className="rounded-lg border border-zinc-200 bg-white px-3 py-1.5 text-sm dark:border-zinc-700 dark:bg-zinc-800"
        >
          <option value="">{t("library.quality.allFormats")}</option>
          {FORMATS.map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
        </select>
      </label>

      {!listLoading && rows.length === 0 ? (
        <p className="py-10 text-center text-sm text-zinc-500 dark:text-zinc-400">
          {listFailed
            ? t("library.quality.loadFailed")
            : t("library.quality.empty")}
        </p>
      ) : (
        <div
          data-track-table
          className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40"
          style={{ minWidth: 720 }}
        >
          <div
            role="row"
            // The library table's header, so every skin dresses both alike.
            className="sticky -top-8 z-20 grid gap-4 px-5 py-3 text-[10px] font-bold tracking-widest text-zinc-400 uppercase border-b border-zinc-100 dark:border-zinc-800 bg-white dark:bg-surface-dark"
            style={{ gridTemplateColumns: GRID }}
          >
            <span aria-hidden="true" />
            <span data-track-header="title">
              {t("library.quality.column.track")}
            </span>
            <span data-track-header="file">
              {t("library.quality.column.file")}
            </span>
            <span data-track-header="highs" className="text-right">
              {t("library.quality.column.highs")}
            </span>
            <span data-track-header="verdict">
              {t("library.quality.column.verdict")}
            </span>
          </div>
          <div
            ref={parentRef}
            className={listLoading ? "opacity-50" : ""}
            style={{
              height: `${virtualizer.getTotalSize()}px`,
              position: "relative",
            }}
          >
            {items.map((virtualRow) => {
              const row = rows[virtualRow.index];
              return (
                <div
                  key={row.id}
                  role="button"
                  tabIndex={0}
                  data-track-row
                  onClick={() => withTrack(row.id, setOpened)}
                  onKeyDown={(e) => {
                    if (e.target !== e.currentTarget) return;
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      withTrack(row.id, setOpened);
                    }
                  }}
                  onContextMenu={(e) => {
                    // Taken now: the track arrives after the event is over.
                    e.preventDefault();
                    withTrack(row.id, (track) => onContextMenu(e, track));
                  }}
                  className="group grid gap-4 px-5 items-center select-none transition-colors cursor-pointer border-b border-zinc-100 dark:border-zinc-800/60 hover:bg-zinc-50 dark:hover:bg-zinc-800/60 focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-emerald-500"
                  style={{
                    position: "absolute",
                    top: 0,
                    left: 0,
                    width: "100%",
                    height: `${virtualRow.size}px`,
                    transform: `translateY(${virtualRow.start - scrollMargin}px)`,
                    gridTemplateColumns: GRID,
                  }}
                >
                  <Artwork
                    path={row.artwork_path}
                    path1x={row.artwork_path_1x}
                    path2x={row.artwork_path_2x}
                    size="1x"
                    className="w-10 h-10"
                    iconSize={18}
                    alt={row.album_title ?? row.title}
                    rounded="md"
                  />
                  <span className="min-w-0">
                    <span className="block truncate text-sm font-medium text-zinc-900 dark:text-white">
                      {row.title}
                    </span>
                    <span className="block truncate text-xs text-zinc-500 dark:text-zinc-400">
                      {[row.artist_name, row.album_title]
                        .filter(Boolean)
                        .join(" · ")}
                    </span>
                  </span>
                  <span className="truncate text-xs tabular-nums text-zinc-500 dark:text-zinc-400">
                    {facts(row) || "—"}
                  </span>
                  <span className="text-right text-sm font-semibold tabular-nums text-zinc-800 dark:text-zinc-100">
                    {highs(row)}
                  </span>
                  <span className="min-w-0">
                    <span
                      data-quality-verdict={row.verdict}
                      className={`inline-block max-w-full truncate rounded-full border px-2.5 py-0.5 text-xs font-medium ${TONES[row.verdict]}`}
                    >
                      {t(`library.quality.verdicts.${row.verdict}`)}
                    </span>
                  </span>
                </div>
              );
            })}
          </div>
        </div>
      )}

      <TrackPropertiesModal
        key={`quality-properties-${opened?.id ?? "none"}`}
        track={opened}
        onClose={() => setOpened(null)}
      />
    </div>
  );
}

/** A verdict chip, styled as the inventory's categories are. */
function chipClass(active: boolean, empty: boolean): string {
  return `flex items-start gap-3 p-3 rounded-xl border text-left transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${
    active
      ? "border-emerald-500 bg-emerald-50 dark:bg-emerald-950/40"
      : "border-zinc-200 dark:border-zinc-800 hover:bg-zinc-50 dark:hover:bg-zinc-800/40"
  } ${empty ? "opacity-50 cursor-default hover:bg-transparent dark:hover:bg-transparent" : ""}`;
}

/**
 * Measures every lossless file again. The first click arms it and the
 * second, within four seconds, confirms: the whole lossless library goes
 * back through the decoder.
 */
function AnalyseAgain({
  t,
  disabled,
  onDone,
}: {
  t: Translator;
  disabled: boolean;
  onDone: () => void;
}) {
  const [armed, setArmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ error: boolean; text: string } | null>(
    null,
  );
  useEffect(() => {
    if (!armed) return;
    const timer = window.setTimeout(() => setArmed(false), CONFIRM_MS);
    return () => window.clearTimeout(timer);
  }, [armed]);

  const confirm = async () => {
    setBusy(true);
    try {
      const queued = await reanalyzeLossless();
      setNotice(
        queued == null
          ? { error: false, text: t("library.quality.alreadyRunning") }
          : { error: false, text: t("library.quality.requested") },
      );
      onDone();
    } catch (err) {
      console.error("[QualityTab] reanalyzeLossless failed", err);
      setNotice({ error: true, text: t("library.quality.requestFailed") });
    } finally {
      setBusy(false);
      setArmed(false);
    }
  };

  return (
    <div className="flex flex-col items-end gap-1">
      <button
        type="button"
        disabled={disabled || busy}
        onClick={() => (armed ? void confirm() : setArmed(true))}
        className={`px-4 py-2 rounded-xl text-sm font-semibold transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${
          armed
            ? "bg-emerald-500 hover:bg-emerald-600 text-white"
            : "border border-zinc-200 bg-white text-zinc-800 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-white dark:hover:bg-zinc-700"
        }`}
      >
        {armed
          ? t("library.quality.confirmAnalyse")
          : t("library.quality.analyseAgain")}
      </button>
      {notice && (
        <p
          role={notice.error ? "alert" : "status"}
          className={`text-xs ${
            notice.error ? "text-red-500" : "text-zinc-500 dark:text-zinc-400"
          }`}
        >
          {notice.text}
        </p>
      )}
    </div>
  );
}

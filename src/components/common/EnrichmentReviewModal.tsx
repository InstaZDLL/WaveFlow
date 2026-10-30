import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  Check,
  ExternalLink,
  Loader2,
  RotateCcw,
  Undo2,
  Wand2,
  X,
} from "lucide-react";

import {
  acceptAlbumMatch,
  acceptArtistMatches,
  ENRICHMENT_CHANGED_EVENT,
  forgetAlbumMatch,
  forgetArtistMatch,
  getAlbumDecisions,
  getArtistDecisions,
  musicBrainzUrl,
  parseGenres,
  rejectAlbumMatch,
  rejectArtistMatch,
  undoAlbumMatch,
  type AlbumDecision,
  type ArtistDecision,
} from "../../lib/tauri/enrichment";
import { openUrl } from "../../lib/tauri/external";
import { useModalA11y } from "../../hooks/useModalA11y";
import { AnimatedModalContent, AnimatedModalShell } from "./AnimatedModalShell";

type Tab = "albums" | "applied" | "artists" | "accepted" | "rejected";

const TABS: readonly Tab[] = [
  "albums",
  "artists",
  "applied",
  "accepted",
  "rejected",
];

/** Estimated row height; rows are measured once drawn. */
const ROW_ESTIMATE = 96;

type Row =
  | { kind: "album"; decision: AlbumDecision }
  | { kind: "artist"; decision: ArtistDecision };

/**
 * The review of what the enrichment engine was not sure about, and the
 * way back from what it applied.
 *
 * Doubtful albums and every artist wait here: an album is shown beside
 * the release it was matched with, an artist beside the MusicBrainz
 * artist and its disambiguation — the line that tells two bands of the
 * same name apart. Accepting looks the release or the artist up again
 * and applies it; rejecting means it is never proposed again on its own;
 * undoing an applied album takes back exactly what was filled.
 *
 * The lists have no natural bound — a large library can wait on
 * hundreds of artists — so they are virtualized against the dialog's own
 * scroller.
 */
export function EnrichmentReviewModal({
  isOpen,
  onClose,
}: {
  isOpen: boolean;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const [tab, setTab] = useState<Tab>("albums");
  const [rows, setRows] = useState<Row[]>([]);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState<Set<string>>(() => new Set());
  const [error, setError] = useState<string | null>(null);
  /** Which load is current: a slow answer for a tab the user left must
   *  not replace the one they are looking at. */
  const loadSeq = useRef(0);

  const load = useCallback(async (which: Tab) => {
    const seq = ++loadSeq.current;
    setLoading(true);
    try {
      let next: Row[];
      if (which === "albums" || which === "applied") {
        const list = await getAlbumDecisions(
          which === "albums" ? "review" : "applied",
        );
        next = list.map((decision) => ({ kind: "album", decision }));
      } else if (which === "rejected") {
        const [albums, artists] = await Promise.all([
          getAlbumDecisions("rejected"),
          getArtistDecisions("rejected"),
        ]);
        next = [
          ...albums.map((decision) => ({ kind: "album" as const, decision })),
          ...artists.map((decision) => ({ kind: "artist" as const, decision })),
        ];
      } else {
        const list = await getArtistDecisions(
          which === "artists" ? "review" : "applied",
        );
        next = list.map((decision) => ({ kind: "artist", decision }));
      }
      if (seq === loadSeq.current) setRows(next);
    } catch (err) {
      if (seq === loadSeq.current)
        setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (seq === loadSeq.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!isOpen) return;
    setError(null);
    void load(tab);
  }, [isOpen, tab, load]);

  // A pass running behind the dialog adds rows; re-read the open tab.
  useEffect(() => {
    if (!isOpen) return;
    let off: (() => void) | undefined;
    let cancelled = false;
    listen(ENRICHMENT_CHANGED_EVENT, () => void load(tab))
      .then((unlisten) => {
        if (cancelled) unlisten();
        else off = unlisten;
      })
      .catch((err) => console.warn("[EnrichmentReview] listen failed", err));
    return () => {
      cancelled = true;
      off?.();
    };
  }, [isOpen, tab, load]);

  // The tab shown now, for a reload that lands after an action: a slow
  // accept must reload the tab the user moved to, not the one they
  // clicked in.
  const tabRef = useRef(tab);
  useEffect(() => {
    tabRef.current = tab;
  }, [tab]);

  const run = async (key: string, action: () => Promise<unknown>) => {
    setBusy((prev) => new Set(prev).add(key));
    setError(null);
    try {
      await action();
      await load(tabRef.current);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy((prev) => {
        const next = new Set(prev);
        next.delete(key);
        return next;
      });
    }
  };

  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_ESTIMATE,
    overscan: 6,
  });

  const pendingArtists =
    tab === "artists"
      ? rows.flatMap((row) =>
          row.kind === "artist" && row.decision.album_evidence > 0
            ? [row.decision.artist_id]
            : [],
        )
      : [];

  return (
    <AnimatedModalShell isOpen={isOpen} onBackdropClick={onClose}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="enrichment-review-title"
        className="relative bg-white dark:bg-surface-dark-elevated text-zinc-900 dark:text-zinc-100 rounded-3xl border border-zinc-200 dark:border-zinc-800 shadow-2xl w-full max-w-4xl h-[85vh] flex flex-col overflow-hidden"
      >
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-200 dark:border-zinc-800">
          <div className="flex items-center gap-2">
            <Wand2 size={18} className="text-emerald-500" aria-hidden="true" />
            <h2 id="enrichment-review-title" className="text-lg font-semibold">
              {t("enrichmentReview.title")}
            </h2>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="p-2 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-full transition-colors"
            aria-label={t("common.close")}
          >
            <X size={18} />
          </button>
        </div>

        <div
          role="tablist"
          aria-label={t("enrichmentReview.title")}
          className="flex flex-wrap gap-1 px-6 pt-3"
        >
          {TABS.map((id) => (
            <button
              key={id}
              type="button"
              role="tab"
              aria-selected={tab === id}
              onClick={() => setTab(id)}
              className={`px-3 py-1.5 rounded-full text-sm transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${
                tab === id
                  ? "bg-emerald-500 text-white"
                  : "text-zinc-600 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800"
              }`}
            >
              {t(`enrichmentReview.tabs.${id}`)}
            </button>
          ))}
        </div>

        <div className="flex items-center justify-between gap-3 px-6 py-2 min-h-10">
          <p className="text-xs text-zinc-500 dark:text-zinc-400">
            {t(`enrichmentReview.hints.${tab}`)}
          </p>
          {pendingArtists.length > 0 && (
            <button
              type="button"
              disabled={busy.has("bulk")}
              onClick={() =>
                void run("bulk", () => acceptArtistMatches(pendingArtists))
              }
              className="shrink-0 flex items-center gap-1.5 px-3 py-1.5 rounded-xl bg-emerald-500 text-white text-xs font-medium hover:bg-emerald-600 disabled:opacity-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
            >
              {busy.has("bulk") ? (
                <Loader2
                  size={12}
                  className="animate-spin"
                  aria-hidden="true"
                />
              ) : (
                <Check size={12} aria-hidden="true" />
              )}
              {t("enrichmentReview.acceptEvidenced", {
                count: pendingArtists.length,
              })}
            </button>
          )}
        </div>

        {error && (
          <p
            role="alert"
            className="mx-6 mb-2 text-xs text-rose-600 dark:text-rose-400"
          >
            {error}
          </p>
        )}

        <div ref={scrollRef} className="flex-1 overflow-y-auto px-6 pb-6">
          {rows.length === 0 ? (
            <div className="py-16 text-center text-sm text-zinc-500">
              {loading ? (
                <Loader2
                  size={18}
                  className="inline animate-spin"
                  aria-label={t("common.loading")}
                />
              ) : (
                t("enrichmentReview.empty")
              )}
            </div>
          ) : (
            <div
              className="relative w-full"
              style={{ height: `${virtualizer.getTotalSize()}px` }}
            >
              {virtualizer.getVirtualItems().map((item) => {
                const row = rows[item.index];
                return (
                  <div
                    key={item.key}
                    data-index={item.index}
                    ref={virtualizer.measureElement}
                    className="absolute left-0 top-0 w-full pb-2"
                    style={{ transform: `translateY(${item.start}px)` }}
                  >
                    {row.kind === "album" ? (
                      <AlbumRow
                        decision={row.decision}
                        busy={busy.has(`album:${row.decision.album_id}`)}
                        onAction={(action) =>
                          void run(`album:${row.decision.album_id}`, () =>
                            action(row.decision.album_id),
                          )
                        }
                      />
                    ) : (
                      <ArtistRow
                        decision={row.decision}
                        busy={busy.has(`artist:${row.decision.artist_id}`)}
                        onAccept={() =>
                          void run(`artist:${row.decision.artist_id}`, () =>
                            acceptArtistMatches([row.decision.artist_id]),
                          )
                        }
                        onReject={() =>
                          void run(`artist:${row.decision.artist_id}`, () =>
                            rejectArtistMatch(row.decision.artist_id),
                          )
                        }
                        onForget={() =>
                          void run(`artist:${row.decision.artist_id}`, () =>
                            forgetArtistMatch(row.decision.artist_id),
                          )
                        }
                      />
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}

function percent(score: number | null): string {
  return score == null ? "—" : `${Math.round(score * 100)} %`;
}

function SmallButton({
  onClick,
  disabled,
  tone,
  icon,
  label,
}: {
  onClick: () => void;
  disabled: boolean;
  tone: "accept" | "neutral";
  icon: ReactNode;
  label: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className={`flex items-center gap-1 px-2.5 py-1 rounded-lg text-xs font-medium transition-colors disabled:opacity-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${
        tone === "accept"
          ? "bg-emerald-500 text-white hover:bg-emerald-600"
          : "border border-zinc-200 dark:border-zinc-700 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-800"
      }`}
    >
      {icon}
      {label}
    </button>
  );
}

function MusicBrainzLink({
  entity,
  mbid,
}: {
  entity: "release" | "artist";
  mbid: string | null;
}) {
  const { t } = useTranslation();
  if (!mbid) return null;
  return (
    <button
      type="button"
      onClick={() =>
        void openUrl(musicBrainzUrl(entity, mbid)).catch((err) =>
          console.warn("[EnrichmentReview] open failed", err),
        )
      }
      className="inline-flex items-center gap-1 text-[11px] text-zinc-500 hover:text-emerald-600 dark:hover:text-emerald-400"
    >
      <ExternalLink size={11} aria-hidden="true" />
      {t("enrichmentReview.openMusicBrainz")}
    </button>
  );
}

function AlbumRow({
  decision,
  busy,
  onAction,
}: {
  decision: AlbumDecision;
  busy: boolean;
  onAction: (action: (albumId: number) => Promise<unknown>) => void;
}) {
  const { t } = useTranslation();
  const genres = parseGenres(decision.genres);
  const local = [
    decision.local_artist,
    t("enrichmentReview.tracks", { count: decision.local_tracks }),
    decision.local_year,
  ]
    .filter((part) => part != null && part !== "")
    .join(" · ");
  const remote = [
    decision.release_artist,
    decision.release_track_count != null
      ? t("enrichmentReview.tracks", { count: decision.release_track_count })
      : null,
    decision.original_date,
    decision.release_country,
  ]
    .filter((part) => part != null && part !== "")
    .join(" · ");
  return (
    <div className="rounded-2xl border border-zinc-200 dark:border-zinc-800 p-3">
      <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
        <div className="min-w-0">
          <div className="text-[11px] uppercase tracking-wide text-zinc-500">
            {t("enrichmentReview.yourAlbum")}
          </div>
          <div className="truncate text-sm font-medium">
            {decision.local_title}
          </div>
          <div className="truncate text-xs text-zinc-500">{local}</div>
        </div>
        <div className="min-w-0">
          <div className="text-[11px] uppercase tracking-wide text-zinc-500">
            {decision.status === "no_match"
              ? t("enrichmentReview.noRelease")
              : t("enrichmentReview.release", {
                  score: percent(decision.score),
                })}
          </div>
          <div className="truncate text-sm font-medium">
            {decision.release_title ?? "—"}
          </div>
          <div className="truncate text-xs text-zinc-500">{remote}</div>
          {genres.length > 0 && (
            <div className="mt-1 flex flex-wrap gap-1">
              {genres.map((genre) => (
                <span
                  key={genre}
                  className="px-1.5 py-0.5 rounded-md bg-zinc-100 dark:bg-zinc-800 text-[11px] text-zinc-600 dark:text-zinc-300"
                >
                  {genre}
                </span>
              ))}
            </div>
          )}
        </div>
      </div>
      <div className="mt-2 flex flex-wrap items-center justify-between gap-2">
        <MusicBrainzLink entity="release" mbid={decision.release_mbid} />
        <div className="flex gap-1.5">
          {decision.status === "review" && (
            <>
              <SmallButton
                tone="neutral"
                disabled={busy}
                onClick={() => onAction(rejectAlbumMatch)}
                icon={<X size={12} aria-hidden="true" />}
                label={t("enrichmentReview.reject")}
              />
              <SmallButton
                tone="accept"
                disabled={busy}
                onClick={() => onAction(acceptAlbumMatch)}
                icon={
                  busy ? (
                    <Loader2
                      size={12}
                      className="animate-spin"
                      aria-hidden="true"
                    />
                  ) : (
                    <Check size={12} aria-hidden="true" />
                  )
                }
                label={t("enrichmentReview.accept")}
              />
            </>
          )}
          {decision.status === "applied" && (
            <SmallButton
              tone="neutral"
              disabled={busy}
              onClick={() => onAction(undoAlbumMatch)}
              icon={<Undo2 size={12} aria-hidden="true" />}
              label={t("enrichmentReview.undo")}
            />
          )}
          {decision.status === "rejected" && (
            <SmallButton
              tone="neutral"
              disabled={busy}
              onClick={() => onAction(forgetAlbumMatch)}
              icon={<RotateCcw size={12} aria-hidden="true" />}
              label={t("enrichmentReview.retry")}
            />
          )}
        </div>
      </div>
    </div>
  );
}

function ArtistRow({
  decision,
  busy,
  onAccept,
  onReject,
  onForget,
}: {
  decision: ArtistDecision;
  busy: boolean;
  onAccept: () => void;
  onReject: () => void;
  onForget: () => void;
}) {
  const { t } = useTranslation();
  const facts = [decision.kind, decision.country]
    .filter((part) => part != null && part !== "")
    .join(" · ");
  return (
    <div className="rounded-2xl border border-zinc-200 dark:border-zinc-800 p-3">
      <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
        <div className="min-w-0">
          <div className="text-[11px] uppercase tracking-wide text-zinc-500">
            {t("enrichmentReview.yourArtist")}
          </div>
          <div className="truncate text-sm font-medium">
            {decision.local_name}
          </div>
        </div>
        <div className="min-w-0">
          <div className="text-[11px] uppercase tracking-wide text-zinc-500">
            {decision.album_evidence > 0
              ? t("enrichmentReview.fromAlbums", {
                  count: decision.album_evidence,
                })
              : t("enrichmentReview.fromName", {
                  score: percent(decision.score),
                })}
          </div>
          <div className="truncate text-sm font-medium">
            {decision.name ?? "—"}
          </div>
          {decision.disambiguation && (
            <div className="truncate text-xs italic text-zinc-500">
              {decision.disambiguation}
            </div>
          )}
          {facts && (
            <div className="truncate text-xs text-zinc-500">{facts}</div>
          )}
        </div>
      </div>
      <div className="mt-2 flex flex-wrap items-center justify-between gap-2">
        <MusicBrainzLink entity="artist" mbid={decision.artist_mbid} />
        <div className="flex gap-1.5">
          {decision.status === "review" && (
            <>
              <SmallButton
                tone="neutral"
                disabled={busy}
                onClick={onReject}
                icon={<X size={12} aria-hidden="true" />}
                label={t("enrichmentReview.reject")}
              />
              <SmallButton
                tone="accept"
                disabled={busy}
                onClick={onAccept}
                icon={
                  busy ? (
                    <Loader2
                      size={12}
                      className="animate-spin"
                      aria-hidden="true"
                    />
                  ) : (
                    <Check size={12} aria-hidden="true" />
                  )
                }
                label={t("enrichmentReview.accept")}
              />
            </>
          )}
          {decision.status === "applied" && (
            <SmallButton
              tone="neutral"
              disabled={busy}
              onClick={onReject}
              icon={<Undo2 size={12} aria-hidden="true" />}
              label={t("enrichmentReview.undo")}
            />
          )}
          {decision.status === "rejected" && (
            <SmallButton
              tone="neutral"
              disabled={busy}
              onClick={onForget}
              icon={<RotateCcw size={12} aria-hidden="true" />}
              label={t("enrichmentReview.retry")}
            />
          )}
        </div>
      </div>
    </div>
  );
}

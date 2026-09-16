import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { X, Check, Loader2, ChevronLeft, Download } from "lucide-react";
import {
  fetchAlbumTagProposals,
  searchAlbumTagSources,
  TAG_FIELDS,
  type AlbumProposals,
  type AlbumSource,
  type TagField,
  type TagValues,
  type TrackProposal,
} from "../../lib/tauri/tagFetch";
import { updateTrackTags, type TrackEdit } from "../../lib/tauri/track";
import { useModalA11y } from "../../hooks/useModalA11y";
import { AnimatedModalContent, AnimatedModalShell } from "./AnimatedModalShell";

/**
 * Fetch an album's tags and approve them, field by field (#599).
 *
 * Three grains of acceptance, because "take all the years but none of
 * the titles" is the common case: one value, one track, or one field
 * across the whole album.
 *
 * Confident matches arrive pre-accepted and doubtful ones do not —
 * that is the whole reason the matcher reports two thresholds instead
 * of one. A doubtful pairing is worth showing and not worth applying
 * for you.
 *
 * Nothing is written until Apply, and Apply goes through
 * `updateTrackTags` one track at a time: the path that pauses
 * playback, writes through the concrete tag so non-standard frames
 * survive, re-hashes and relinks. A track that fails is counted and
 * the rest still run — stopping halfway through an album is what
 * leaves a folder in a state nobody can describe.
 */
interface TagFetchModalProps {
  isOpen: boolean;
  onClose: () => void;
  albumId: number;
  /** Called after at least one track was written, so the view can
   *  refetch — the rows on screen are now stale. */
  onApplied?: () => void;
}

/** Which fields of which tracks the user has accepted. */
type Accepted = Record<number, Partial<Record<TagField, boolean>>>;

export function TagFetchModal({
  isOpen,
  onClose,
  albumId,
  onApplied,
}: TagFetchModalProps) {
  const { t } = useTranslation();
  /** A catalogue round-trip is in flight. Shows a spinner, nothing more. */
  const [isLoading, setIsLoading] = useState(false);
  /** Files are being written. This is the one that locks the modal. */
  const [isApplying, setIsApplying] = useState(false);
  /**
   * Dismissal, unless a **write** is in flight.
   *
   * Apply walks the album one file at a time and closing does not stop
   * it: the loop keeps writing into a folder whose review screen is
   * gone, and the summary of what was written and what failed is lost
   * with it. Every path is routed through here — the X, the footer
   * button, the backdrop, and the Escape key `useModalA11y` binds.
   *
   * A *fetch* in flight does not lock anything. Two Deezer calls can
   * take ten seconds between them, and a modal that refuses Escape
   * while it waits for a network it may never hear from is worse than
   * one whose answer arrives to nobody.
   *
   * A reply that lands after the close is dropped by the token, which
   * the effect below retires on the way out.
   */
  const closeUnlessBusy = () => {
    if (!isApplying) onClose();
  };
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, closeUnlessBusy);
  /**
   * Which fetch is the current one.
   *
   * Claimed when a fetch starts, retired when the modal closes, and
   * checked when the reply lands — so an answer to a release the user
   * has already left, or to a modal they have closed, is dropped
   * instead of appearing under the wrong record.
   */
  const fetchTokenRef = useRef(0);
  const [sources, setSources] = useState<AlbumSource[] | null>(null);
  const [proposals, setProposals] = useState<AlbumProposals | null>(null);
  const [accepted, setAccepted] = useState<Accepted>({});
  const [error, setError] = useState<string | null>(null);
  const [applied, setApplied] = useState<{
    ok: number;
    failed: number;
  } | null>(null);

  useEffect(() => {
    if (!isOpen) {
      // Retire the token on the way out, and here rather than in the
      // close handler for two reasons: the handler is passed to
      // `useModalA11y`, and a ref mutated inside a hook's argument is
      // one the lint refuses to see mutated anywhere; and a close the
      // parent decided — navigating away from the album — goes through
      // no handler of ours at all.
      //
      // Bumping it only when a fetch *starts* left a window: a reply
      // landing between the close and the next opening still passed
      // its own check and wrote its proposals into state, and the next
      // opening rendered them for a frame before the reset below
      // cleared them — the previous album's track list, under the new
      // album's name.
      fetchTokenRef.current += 1;
      return;
    }
    let alive = true;
    /* eslint-disable react-hooks/set-state-in-effect */
    setSources(null);
    setProposals(null);
    setAccepted({});
    setApplied(null);
    setError(null);
    /* eslint-enable react-hooks/set-state-in-effect */
    const token = ++fetchTokenRef.current;
    searchAlbumTagSources(albumId)
      .then((s) => {
        if (alive && fetchTokenRef.current === token) setSources(s);
      })
      .catch((err) => {
        if (!alive || fetchTokenRef.current !== token) return;
        console.error("[TagFetch] source search failed", err);
        setError(String(err));
        setSources([]);
      });
    return () => {
      alive = false;
    };
  }, [isOpen, albumId]);

  const pickSource = async (source: AlbumSource) => {
    const token = ++fetchTokenRef.current;
    setIsLoading(true);
    setError(null);
    try {
      const result = await fetchAlbumTagProposals(albumId, source.deezer_id);
      if (fetchTokenRef.current !== token) return;
      setProposals(result);
      setAccepted(defaultAcceptance(result));
    } catch (err) {
      if (fetchTokenRef.current !== token) return;
      console.error("[TagFetch] proposals failed", err);
      setError(String(err));
    } finally {
      if (fetchTokenRef.current === token) setIsLoading(false);
    }
  };

  const toggle = (trackId: number, field: TagField) => {
    setAccepted((prev) => ({
      ...prev,
      [trackId]: { ...prev[trackId], [field]: !prev[trackId]?.[field] },
    }));
  };

  const toggleTrack = (proposal: TrackProposal) => {
    const fields = changedFields(proposal);
    const allOn = fields.every((f) => accepted[proposal.track_id]?.[f]);
    setAccepted((prev) => ({
      ...prev,
      [proposal.track_id]: Object.fromEntries(
        fields.map((f) => [f, !allOn]),
      ) as Partial<Record<TagField, boolean>>,
    }));
  };

  const toggleColumn = (field: TagField) => {
    if (!proposals) return;
    const rows = proposals.tracks.filter((p) =>
      changedFields(p).includes(field),
    );
    const allOn = rows.every((p) => accepted[p.track_id]?.[field]);
    setAccepted((prev) => {
      const next = { ...prev };
      for (const p of rows) {
        next[p.track_id] = { ...next[p.track_id], [field]: !allOn };
      }
      return next;
    });
  };

  const pendingCount = proposals
    ? proposals.tracks.reduce(
        (sum, p) =>
          sum +
          changedFields(p).filter((f) => accepted[p.track_id]?.[f]).length,
        0,
      )
    : 0;

  const apply = async () => {
    if (!proposals || pendingCount === 0) return;
    setIsApplying(true);
    setError(null);
    let ok = 0;
    let failed = 0;
    for (const proposal of proposals.tracks) {
      const fields = changedFields(proposal).filter(
        (f) => accepted[proposal.track_id]?.[f],
      );
      if (fields.length === 0) continue;
      const edit: TrackEdit = {};
      for (const field of fields) {
        // Only the accepted fields are sent: `update_track_tags`
        // leaves an omitted field alone, which is what makes accepting
        // one value of one track mean exactly that.
        Object.assign(edit, editFragment(proposal, field));
      }
      try {
        await updateTrackTags(proposal.track_id, edit);
        ok += 1;
      } catch (err) {
        console.error("[TagFetch] write failed", proposal.track_id, err);
        failed += 1;
      }
    }
    setApplied({ ok, failed });
    setIsApplying(false);
    if (ok > 0) onApplied?.();
  };

  return (
    <AnimatedModalShell isOpen={isOpen} onBackdropClick={closeUnlessBusy}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="tag-fetch-title"
        className="relative bg-white dark:bg-surface-dark-elevated text-zinc-900 dark:text-zinc-100 rounded-3xl border border-zinc-200 dark:border-zinc-800 shadow-2xl w-full max-w-4xl max-h-[90vh] flex flex-col overflow-hidden"
      >
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-200 dark:border-zinc-800">
          <div className="flex items-center gap-2">
            {proposals && !applied && (
              <button
                type="button"
                onClick={() => {
                  setProposals(null);
                  setAccepted({});
                }}
                className="p-1.5 rounded-full hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
                aria-label={t("tagFetch.back")}
              >
                <ChevronLeft size={18} />
              </button>
            )}
            <Download size={18} className="text-sky-500" />
            <h2 id="tag-fetch-title" className="text-lg font-semibold">
              {t("tagFetch.title")}
            </h2>
          </div>
          <button
            type="button"
            onClick={closeUnlessBusy}
            disabled={isApplying}
            className="p-2 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-full transition-colors disabled:opacity-50"
            aria-label={t("common.close")}
          >
            <X size={18} />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto p-6 space-y-4">
          {applied ? (
            <AppliedSummary applied={applied} />
          ) : proposals ? (
            <ReviewList
              proposals={proposals}
              accepted={accepted}
              onToggle={toggle}
              onToggleTrack={toggleTrack}
              onToggleColumn={toggleColumn}
            />
          ) : (
            <SourceList
              sources={sources}
              busy={isLoading}
              onPick={pickSource}
            />
          )}
          {error && <p className="text-xs text-red-500">{error}</p>}
        </div>

        <div className="flex items-center justify-between gap-3 px-6 py-4 border-t border-zinc-200 dark:border-zinc-800">
          <span className="text-xs text-zinc-500 dark:text-zinc-400">
            {proposals && !applied && t("tagFetch.hint")}
          </span>
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={closeUnlessBusy}
              disabled={isApplying}
              className="px-4 py-2 rounded-full text-sm hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors disabled:opacity-50"
            >
              {applied ? t("common.close") : t("common.cancel")}
            </button>
            {proposals && !applied && (
              <button
                type="button"
                onClick={() => void apply()}
                disabled={isApplying || pendingCount === 0}
                className="px-5 py-2 rounded-full bg-zinc-900 dark:bg-white text-white dark:text-zinc-900 text-sm font-medium hover:opacity-90 disabled:opacity-50 transition-opacity flex items-center gap-2"
              >
                {isApplying && <Loader2 size={14} className="animate-spin" />}
                {pendingCount === 0
                  ? t("tagFetch.apply")
                  : t("tagFetch.applyCount", { count: pendingCount })}
              </button>
            )}
          </div>
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}

// =============================================================================
// Steps
// =============================================================================

function SourceList({
  sources,
  busy,
  onPick,
}: {
  sources: AlbumSource[] | null;
  busy: boolean;
  onPick: (s: AlbumSource) => void;
}) {
  const { t } = useTranslation();
  if (sources == null || busy) {
    return (
      <p className="flex items-center gap-2 text-sm text-zinc-500 dark:text-zinc-400">
        <Loader2 size={14} className="animate-spin" />
        {busy ? t("tagFetch.matching") : t("tagFetch.searching")}
      </p>
    );
  }
  if (sources.length === 0) {
    return (
      <p className="text-sm text-zinc-500 dark:text-zinc-400">
        {t("tagFetch.noSources")}
      </p>
    );
  }
  return (
    <>
      <p className="text-sm text-zinc-500 dark:text-zinc-400">
        {t("tagFetch.sourcesHint")}
      </p>
      <ul className="space-y-2">
        {sources.map((source) => (
          <li key={source.deezer_id}>
            <button
              type="button"
              onClick={() => onPick(source)}
              className="w-full flex items-center gap-3 p-3 rounded-xl border border-zinc-200 dark:border-zinc-700 hover:bg-zinc-50 dark:hover:bg-zinc-800/60 transition-colors text-left"
            >
              {source.cover_url ? (
                <img
                  src={source.cover_url}
                  alt=""
                  className="w-12 h-12 rounded-lg object-cover shrink-0"
                  loading="lazy"
                />
              ) : (
                <div className="w-12 h-12 rounded-lg bg-zinc-100 dark:bg-zinc-800 shrink-0" />
              )}
              <span className="min-w-0 flex-1">
                <span className="block font-medium truncate">
                  {source.title}
                </span>
                <span className="block text-xs text-zinc-500 dark:text-zinc-400 truncate">
                  {[
                    source.artist,
                    source.year?.toString(),
                    source.track_count != null
                      ? t("tagFetch.trackCount", { count: source.track_count })
                      : null,
                  ]
                    .filter(Boolean)
                    .join(" · ")}
                </span>
              </span>
            </button>
          </li>
        ))}
      </ul>
    </>
  );
}

function ReviewList({
  proposals,
  accepted,
  onToggle,
  onToggleTrack,
  onToggleColumn,
}: {
  proposals: AlbumProposals;
  accepted: Accepted;
  onToggle: (trackId: number, field: TagField) => void;
  onToggleTrack: (p: TrackProposal) => void;
  onToggleColumn: (field: TagField) => void;
}) {
  const { t } = useTranslation();
  const changedAnywhere = TAG_FIELDS.filter((field) =>
    proposals.tracks.some((p) => changedFields(p).includes(field)),
  );

  if (changedAnywhere.length === 0) {
    return (
      <p className="text-sm text-zinc-500 dark:text-zinc-400">
        {t("tagFetch.nothingToChange")}
      </p>
    );
  }

  return (
    <>
      {/* One field across the whole album — the grain that makes "all
          the years, none of the titles" one click instead of twelve. */}
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-xs text-zinc-500 dark:text-zinc-400">
          {t("tagFetch.acceptColumn")}
        </span>
        {changedAnywhere.map((field) => (
          <button
            key={field}
            type="button"
            onClick={() => onToggleColumn(field)}
            className="px-2.5 py-1 text-xs rounded-md bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 transition-colors"
          >
            {t(`tagFetch.fields.${field}`)}
          </button>
        ))}
      </div>

      <ul className="space-y-2">
        {proposals.tracks.map((proposal) => {
          const fields = changedFields(proposal);
          return (
            <li
              key={proposal.track_id}
              className="rounded-xl border border-zinc-200 dark:border-zinc-700 p-3"
            >
              <div className="flex items-center gap-2 flex-wrap">
                <span className="font-medium text-sm truncate min-w-0 flex-1">
                  {proposal.current.title || proposal.file_name}
                </span>
                {proposal.confidence && (
                  <span
                    className={`text-[10px] font-semibold uppercase tracking-wide px-1.5 py-0.5 rounded ${
                      proposal.confidence === "confident"
                        ? "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400"
                        : "bg-amber-500/15 text-amber-600 dark:text-amber-400"
                    }`}
                  >
                    {t(`tagFetch.confidence.${proposal.confidence}`)}
                  </span>
                )}
                {fields.length > 0 && (
                  <button
                    type="button"
                    onClick={() => onToggleTrack(proposal)}
                    className="text-xs px-2 py-1 rounded-md hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
                  >
                    {t("tagFetch.acceptTrack")}
                  </button>
                )}
              </div>

              {proposal.fetched == null ? (
                <p className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
                  {t("tagFetch.noMatch", { file: proposal.file_name })}
                </p>
              ) : fields.length === 0 ? (
                <p className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
                  {t("tagFetch.trackUnchanged")}
                </p>
              ) : (
                <ul className="mt-2 space-y-1">
                  {fields.map((field) => (
                    <li key={field} className="flex items-center gap-2 text-xs">
                      <input
                        type="checkbox"
                        id={`tf-${proposal.track_id}-${field}`}
                        checked={Boolean(accepted[proposal.track_id]?.[field])}
                        onChange={() => onToggle(proposal.track_id, field)}
                        className="accent-sky-500"
                      />
                      <label
                        htmlFor={`tf-${proposal.track_id}-${field}`}
                        className="flex items-center gap-2 min-w-0 flex-1 cursor-pointer"
                      >
                        <span className="w-24 shrink-0 text-zinc-500 dark:text-zinc-400">
                          {t(`tagFetch.fields.${field}`)}
                        </span>
                        <span className="truncate line-through text-zinc-400 dark:text-zinc-500">
                          {display(proposal.current, field) ||
                            t("tagFetch.emptyValue")}
                        </span>
                        <span className="text-zinc-400">→</span>
                        <span className="truncate font-medium">
                          {/* `fields` is empty unless `fetched` is
                              there, so this branch cannot be reached
                              without it — narrowed explicitly rather
                              than asserted. */}
                          {(proposal.fetched
                            ? display(proposal.fetched, field)
                            : "") || t("tagFetch.emptyValue")}
                        </span>
                      </label>
                    </li>
                  ))}
                </ul>
              )}
            </li>
          );
        })}
      </ul>

      {proposals.unmatched_remote.length > 0 && (
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          {t("tagFetch.unmatchedRemote", {
            titles: proposals.unmatched_remote.join(", "),
          })}
        </p>
      )}
    </>
  );
}

function AppliedSummary({
  applied,
}: {
  applied: { ok: number; failed: number };
}) {
  const { t } = useTranslation();
  return (
    <div className="space-y-2">
      <p className="flex items-center gap-2 text-sm">
        <Check size={16} className="text-emerald-500" />
        {t("tagFetch.appliedCount", { count: applied.ok })}
      </p>
      {applied.failed > 0 && (
        <p className="text-sm text-amber-600 dark:text-amber-400">
          {t("tagFetch.failedCount", { count: applied.failed })}
        </p>
      )}
    </div>
  );
}

// =============================================================================
// Field helpers
// =============================================================================

/** The fields where the catalogue says something different. */
function changedFields(proposal: TrackProposal): TagField[] {
  if (!proposal.fetched) return [];
  const fetched = proposal.fetched;
  return TAG_FIELDS.filter((field) => {
    const next = fetched[field];
    // A field the catalogue does not carry is not a change to nothing:
    // offering it would invite replacing a value the user typed with a
    // blank.
    if (next == null || next === "") return false;
    return String(next) !== String(proposal.current[field] ?? "");
  });
}

function display(values: TagValues, field: TagField): string {
  const value = values[field];
  return value == null ? "" : String(value);
}

/** The `TrackEdit` fragment for one accepted field. */
function editFragment(proposal: TrackProposal, field: TagField): TrackEdit {
  const value = proposal.fetched?.[field];
  switch (field) {
    case "title":
      return { title: value as string };
    case "artist":
      return { artist: value as string };
    case "album":
      return { album: value as string };
    case "year":
      return { year: value as number };
    case "track_number":
      return { track_number: value as number };
  }
}

/** The library's one spelling for a multi-artist credit. */
const MULTI_ARTIST_SEPARATOR = "; ";

/**
 * What arrives pre-accepted.
 *
 * Confident matches, and only those: a doubtful pairing is worth
 * showing and not worth applying on the user's behalf, which is the
 * entire reason the matcher reports two thresholds rather than one.
 *
 * With one exception. Deezer gives a track **one** artist, and a local
 * credit of "A; B" therefore always reads as a change — so a confident
 * match would arrive with "replace both names with the first" already
 * ticked. The row stays visible and can still be accepted by hand; it
 * is only the default that refuses to throw away a credit the library
 * models better than the catalogue does.
 */
function defaultAcceptance(proposals: AlbumProposals): Accepted {
  const out: Accepted = {};
  for (const proposal of proposals.tracks) {
    if (proposal.confidence !== "confident") continue;
    const multiArtist = (proposal.current.artist ?? "").includes(
      MULTI_ARTIST_SEPARATOR,
    );
    const fields = changedFields(proposal).filter(
      (f) => !(f === "artist" && multiArtist),
    );
    if (fields.length === 0) continue;
    out[proposal.track_id] = Object.fromEntries(
      fields.map((f) => [f, true]),
    ) as Partial<Record<TagField, boolean>>;
  }
  return out;
}

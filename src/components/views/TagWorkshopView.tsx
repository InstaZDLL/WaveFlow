import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import {
  AlertTriangle,
  CaseSensitive,
  Check,
  Eye,
  ListOrdered,
  Loader2,
  PaintBucket,
  Replace,
  RotateCcw,
  Square,
  Tags,
  Undo2,
  Wand2,
} from "lucide-react";

import { usePageVirtualizer } from "../../hooks/usePageVirtualizer";
import { useTasks } from "../../hooks/useTasks";
import { cancelTask } from "../../lib/tauri/tasks";
import { openUrl } from "../../lib/tauri/external";
import { musicBrainzUrl } from "../../lib/tauri/enrichment";
import {
  applyWorkshop,
  IDENTIFIER_FIELDS,
  listWorkshopBatches,
  matchWorkshop,
  NUMBER_FIELDS,
  openWorkshop,
  parseReleaseMbid,
  previewWorkshop,
  TAG_WORKSHOP_CHANGED_EVENT,
  undoWorkshopBatch,
  WORKSHOP_FIELDS,
  type AlbumProposal,
  type ApplyReport,
  type BatchSummary,
  type FilePreview,
  type TrackChange,
  type UndoReport,
  type WorkshopField,
  type WorkshopRow,
  type WorkshopValues,
} from "../../lib/tauri/tagWorkshop";
import {
  applyCase,
  applyFindReplace,
  compileFind,
  TEXT_FIELDS,
  type CaseMode,
  type TagWorkshopRequest,
} from "../../lib/tagWorkshop";
import { EmptyState } from "../common/EmptyState";
import { WorkshopPreviewModal } from "./tagWorkshop/WorkshopPreviewModal";

type Group = "tags" | "numbers" | "ids";

const GROUPS: Record<Group, readonly WorkshopField[]> = {
  tags: [
    "title",
    "artist",
    "album",
    "album_artist",
    "year",
    "genre",
    "composer",
  ],
  numbers: ["track_number", "track_total", "disc_number", "disc_total"],
  ids: IDENTIFIER_FIELDS,
};

const ROW_HEIGHT = 40;

type Edits = Map<number, WorkshopValues>;
type Tool = "fill" | "replace" | "case" | "number" | "match";

function basename(path: string): string {
  const cut = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return cut >= 0 ? path.slice(cut + 1) : path;
}

/** The most common non-empty value among `values`, for prefilling. */
function mostCommon(values: string[]): string {
  const counts = new Map<string, number>();
  for (const v of values) {
    const key = v.trim();
    if (key) counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  let best = "";
  let bestCount = 0;
  for (const [value, count] of counts) {
    if (count > bestCount) {
      best = value;
      bestCount = count;
    }
  }
  return best;
}

/** Every column shares the width the view has: a cell truncates rather
 *  than push the page into scrolling sideways. */
function columnWidth(field: WorkshopField): string {
  if (NUMBER_FIELDS.includes(field)) return "3.25rem";
  return "minmax(0, 1fr)";
}

/**
 * The tag workshop: a selection, an album or a folder in one table,
 * edited in place and with tools that act on a column — fill, find and
 * replace, case, numbering — or matched with a MusicBrainz release.
 *
 * Nothing in the table is written as it is typed. "Preview" asks the
 * backend what each file says **now** beside what the table asks, field
 * by field, and only that preview's "Write" writes — as one journaled
 * batch the list at the bottom can undo.
 *
 * The rows are virtualized against the page scroller: a folder can hold
 * thousands of files, and the view has no scroller of its own.
 */
export function TagWorkshopView({
  request,
}: {
  request: TagWorkshopRequest | null;
}) {
  const { t, i18n } = useTranslation();
  const [rows, setRows] = useState<WorkshopRow[]>([]);
  // True from the start: the first read is under way as soon as the view
  // mounts with a source.
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [edits, setEdits] = useState<Edits>(() => new Map());
  const [selected, setSelected] = useState<Set<number>>(() => new Set());
  const [groups, setGroups] = useState<Set<Group>>(
    () => new Set<Group>(["tags", "numbers"]),
  );
  // Opened from the enrichment review with a release: the match is what
  // the user came for, so its panel is open from the start.
  const [tool, setTool] = useState<Tool | null>(
    request?.releaseMbid ? "match" : null,
  );
  const [proposal, setProposal] = useState<AlbumProposal | null>(null);
  const [preview, setPreview] = useState<FilePreview[] | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [writing, setWriting] = useState(false);
  const [report, setReport] = useState<ApplyReport | null>(null);
  const [batches, setBatches] = useState<BatchSummary[]>([]);
  const [undoReport, setUndoReport] = useState<UndoReport | null>(null);
  const tasks = useTasks();
  const workshopTask = tasks.find((task) => task.kind === "tag_workshop");

  const source = request?.source ?? null;

  const reload = useCallback(async () => {
    if (!source) return;
    setLoading(true);
    setError(null);
    try {
      setRows(await openWorkshop(source));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, [source]);

  const reloadBatches = useCallback(async () => {
    try {
      setBatches(await listWorkshopBatches());
    } catch (err) {
      console.warn("[TagWorkshop] batches failed", err);
    }
  }, []);

  // The first read, with its state set from the answer only: `reload` is
  // what a write or an undo calls afterwards.
  useEffect(() => {
    let cancelled = false;
    if (source) {
      openWorkshop(source)
        .then((next) => {
          if (!cancelled) setRows(next);
        })
        .catch((err: unknown) => {
          if (!cancelled)
            setError(err instanceof Error ? err.message : String(err));
        })
        .finally(() => {
          if (!cancelled) setLoading(false);
        });
    }
    listWorkshopBatches()
      .then((list) => {
        if (!cancelled) setBatches(list);
      })
      .catch((err: unknown) =>
        console.warn("[TagWorkshop] batches failed", err),
      );
    return () => {
      cancelled = true;
    };
  }, [source]);

  useEffect(() => {
    let off: (() => void) | undefined;
    let cancelled = false;
    listen(TAG_WORKSHOP_CHANGED_EVENT, () => void reloadBatches())
      .then((unlisten) => {
        if (cancelled) unlisten();
        else off = unlisten;
      })
      .catch((err) => console.warn("[TagWorkshop] listen failed", err));
    return () => {
      cancelled = true;
      off?.();
    };
  }, [reloadBatches]);

  const editable = useMemo(() => rows.filter((row) => !row.error), [rows]);
  /** What a tool acts on: the ticked rows, or every editable one. */
  const targets = useMemo(
    () =>
      selected.size > 0
        ? editable.filter((row) => selected.has(row.track_id))
        : editable,
    [editable, selected],
  );

  const valueOf = useCallback(
    (row: WorkshopRow, field: WorkshopField): string =>
      edits.get(row.track_id)?.[field] ?? row.values[field] ?? "",
    [edits],
  );

  const isEdited = (row: WorkshopRow, field: WorkshopField): boolean => {
    const edited = edits.get(row.track_id)?.[field];
    return edited != null && edited.trim() !== (row.values[field] ?? "").trim();
  };

  /** Set `field` on each of `targets` to what `next` makes of it. */
  const editMany = useCallback(
    (
      list: WorkshopRow[],
      field: WorkshopField,
      next: (current: string, row: WorkshopRow, index: number) => string,
    ) => {
      setEdits((prev) => {
        const out = new Map(prev);
        list.forEach((row, index) => {
          const current =
            prev.get(row.track_id)?.[field] ?? row.values[field] ?? "";
          const value = next(current, row, index);
          out.set(row.track_id, {
            ...(out.get(row.track_id) ?? {}),
            [field]: value,
          });
        });
        return out;
      });
      setReport(null);
    },
    [],
  );

  const setCell = (row: WorkshopRow, field: WorkshopField, value: string) =>
    editMany([row], field, () => value);

  /** Every row's edits that differ from the file, as the backend takes
   *  them. */
  const changes: TrackChange[] = useMemo(() => {
    const out: TrackChange[] = [];
    for (const row of editable) {
      const edit = edits.get(row.track_id);
      if (!edit) continue;
      const values: WorkshopValues = {};
      for (const field of WORKSHOP_FIELDS) {
        const v = edit[field];
        if (v == null) continue;
        if (v.trim() === (row.values[field] ?? "").trim()) continue;
        values[field] = v;
      }
      if (Object.keys(values).length > 0)
        out.push({ track_id: row.track_id, values });
    }
    return out;
  }, [editable, edits]);

  const visibleFields = useMemo(
    () =>
      (["tags", "numbers", "ids"] as Group[])
        .filter((g) => groups.has(g))
        .flatMap((g) => GROUPS[g]),
    [groups],
  );
  const gridTemplate = `1.5rem minmax(0, 1.2fr) ${visibleFields
    .map(columnWidth)
    .join(" ")}`;

  const { parentRef, virtualizer, scrollMargin } = usePageVirtualizer({
    count: rows.length,
    estimateSize: () => ROW_HEIGHT,
    overscan: 12,
  });

  const runPreview = async () => {
    setPreviewing(true);
    setError(null);
    try {
      setPreview(await previewWorkshop(changes));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setPreviewing(false);
    }
  };

  const write = async (label: string, withCover: boolean) => {
    if (!preview) return;
    // Exactly what the preview showed: the files it listed, the new
    // values it listed, and each old value as the write's check.
    const toWrite: TrackChange[] = preview
      .filter((p) => p.diffs.length > 0 && !p.invalid && !p.error)
      .map((p) => ({
        track_id: p.track_id,
        values: Object.fromEntries(p.diffs.map((d) => [d.field, d.new])),
        expected: Object.fromEntries(p.diffs.map((d) => [d.field, d.old])),
      }));
    const releaseMbid = proposal?.release.mbid ?? null;
    const cover =
      withCover && proposal
        ? {
            release_mbid: proposal.release.mbid,
            release_group_mbid: proposal.release.release_group_mbid,
            has_front_cover: proposal.release.has_front_cover,
          }
        : null;
    setWriting(true);
    setPreview(null);
    setUndoReport(null);
    try {
      const result = await applyWorkshop(toWrite, label, releaseMbid, cover);
      setReport(result);
      // What was written is the files' now, and so is what the backend
      // found already so: those edits are done. A refused file keeps its
      // edits, and so does every file a stopped batch never reached. An
      // edit the preview found no difference in ("01" for "1") is done
      // too: it would only ever preview as nothing.
      const done = new Set(result.done);
      const previewed = new Set(preview.map((p) => p.track_id));
      setEdits((prev) => {
        const out = new Map<number, WorkshopValues>();
        for (const [trackId, values] of prev) {
          if (done.has(trackId)) continue;
          if (!previewed.has(trackId) && !result.cancelled) continue;
          out.set(trackId, values);
        }
        return out;
      });
      await reload();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setWriting(false);
    }
  };

  const undo = async (batchId: number) => {
    setWriting(true);
    setError(null);
    // The last write's report is about a batch this may be undoing.
    setReport(null);
    try {
      setUndoReport(await undoWorkshopBatch(batchId));
      await reload();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setWriting(false);
      void reloadBatches();
    }
  };

  const takeProposal = (fields: Set<WorkshopField>) => {
    if (!proposal) return;
    const byId = new Map(rows.map((row) => [row.track_id, row]));
    setEdits((prev) => {
      const out = new Map(prev);
      for (const track of proposal.tracks) {
        const row = byId.get(track.track_id);
        if (!row || row.error) continue;
        const next = { ...(out.get(track.track_id) ?? {}) };
        for (const [field, value] of Object.entries(track.values) as [
          WorkshopField,
          string,
        ][]) {
          if (fields.has(field)) next[field] = value;
        }
        out.set(track.track_id, next);
      }
      return out;
    });
    setReport(null);
  };

  const allSelected =
    editable.length > 0 && editable.every((row) => selected.has(row.track_id));

  if (!source) {
    return (
      <EmptyState
        icon={<Tags size={28} />}
        title={t("tagWorkshop.title")}
        description={t("tagWorkshop.emptyHint")}
      />
    );
  }

  return (
    <div className="mx-auto max-w-[1600px] space-y-5 animate-fade-in pb-20">
      <header className="flex flex-wrap items-end justify-between gap-4">
        <div className="min-w-0">
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
            {t("tagWorkshop.eyebrow")}
          </div>
          <h1 className="text-3xl font-bold text-zinc-900 dark:text-white">
            {t("tagWorkshop.title")}
          </h1>
          <p className="text-sm text-zinc-500 mt-1">
            {t("tagWorkshop.summary", {
              count: rows.length,
              changed: changes.length,
            })}
            {selected.size > 0 &&
              ` · ${t("tagWorkshop.selectedCount", { count: selected.size })}`}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {workshopTask && (
            <button
              type="button"
              onClick={() => void cancelTask(workshopTask.id)}
              disabled={workshopTask.cancelling}
              className="flex items-center gap-1.5 px-3 py-2 rounded-xl border border-zinc-200 dark:border-zinc-700 text-sm text-zinc-700 dark:text-zinc-200 hover:bg-zinc-50 dark:hover:bg-zinc-800 disabled:opacity-50"
            >
              <Square size={14} aria-hidden="true" />
              {t("tagWorkshop.stop", {
                current: workshopTask.current,
                total: workshopTask.total,
              })}
            </button>
          )}
          <button
            type="button"
            disabled={changes.length === 0 || writing}
            onClick={() => setEdits(new Map())}
            className="flex items-center gap-1.5 px-3 py-2 rounded-xl border border-zinc-200 dark:border-zinc-700 text-sm text-zinc-700 dark:text-zinc-200 hover:bg-zinc-50 dark:hover:bg-zinc-800 disabled:opacity-50"
          >
            <RotateCcw size={14} aria-hidden="true" />
            {t("tagWorkshop.discard")}
          </button>
          <button
            type="button"
            disabled={changes.length === 0 || previewing || writing}
            onClick={() => void runPreview()}
            className="flex items-center gap-1.5 px-4 py-2 rounded-xl bg-emerald-500 text-white text-sm font-medium hover:bg-emerald-600 disabled:opacity-50"
          >
            {previewing || writing ? (
              <Loader2 size={14} className="animate-spin" aria-hidden="true" />
            ) : (
              <Eye size={14} aria-hidden="true" />
            )}
            {t("tagWorkshop.preview", { count: changes.length })}
          </button>
        </div>
      </header>

      <div className="flex flex-wrap items-center gap-2">
        {(
          [
            ["fill", PaintBucket],
            ["replace", Replace],
            ["case", CaseSensitive],
            ["number", ListOrdered],
            ["match", Wand2],
          ] as const
        ).map(([id, Icon]) => (
          <button
            key={id}
            type="button"
            aria-pressed={tool === id}
            onClick={() => setTool(tool === id ? null : id)}
            className={`flex items-center gap-1.5 px-3 py-1.5 rounded-full text-sm transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${
              tool === id
                ? "bg-emerald-500 text-white"
                : "bg-zinc-100 dark:bg-zinc-800 text-zinc-700 dark:text-zinc-200 hover:bg-zinc-200 dark:hover:bg-zinc-700"
            }`}
          >
            <Icon size={14} aria-hidden="true" />
            {t(`tagWorkshop.tools.${id}`)}
          </button>
        ))}
        <span className="mx-2 h-5 w-px bg-zinc-200 dark:bg-zinc-700" />
        {(["tags", "numbers", "ids"] as Group[]).map((group) => (
          <label
            key={group}
            className="flex items-center gap-1.5 text-sm text-zinc-600 dark:text-zinc-300"
          >
            <input
              type="checkbox"
              checked={groups.has(group)}
              onChange={() =>
                setGroups((prev) => {
                  const next = new Set(prev);
                  if (next.has(group)) next.delete(group);
                  else next.add(group);
                  return next;
                })
              }
            />
            {t(`tagWorkshop.groups.${group}`)}
          </label>
        ))}
      </div>

      {tool && (
        <div className="rounded-2xl border border-zinc-200 dark:border-zinc-800 p-4">
          <p className="mb-3 text-xs text-zinc-500">
            {selected.size > 0
              ? t("tagWorkshop.appliesToSelection", { count: targets.length })
              : t("tagWorkshop.appliesToAll", { count: targets.length })}
          </p>
          {tool === "fill" && (
            <FillTool
              onApply={(field, value) => editMany(targets, field, () => value)}
            />
          )}
          {tool === "replace" && (
            <ReplaceTool
              onApply={(fields, pattern, spec) => {
                for (const field of fields) {
                  editMany(targets, field, (current) =>
                    applyFindReplace(current, pattern, spec),
                  );
                }
              }}
            />
          )}
          {tool === "case" && (
            <CaseTool
              onApply={(fields, mode) => {
                for (const field of fields) {
                  editMany(targets, field, (current) =>
                    applyCase(current, mode),
                  );
                }
              }}
            />
          )}
          {tool === "number" && (
            <NumberTool
              onApply={(start, perDisc, totals) => {
                // In table order; with `perDisc`, each disc counts from
                // `start` again and gets its own total.
                const discOf = (row: WorkshopRow) =>
                  perDisc ? valueOf(row, "disc_number").trim() || "1" : "";
                const positions = new Map<number, number>();
                const sizes = new Map<string, number>();
                const seen = new Map<string, number>();
                for (const row of targets) {
                  const disc = discOf(row);
                  const at = seen.get(disc) ?? 0;
                  positions.set(row.track_id, at);
                  seen.set(disc, at + 1);
                }
                for (const [disc, count] of seen) sizes.set(disc, count);
                editMany(targets, "track_number", (_c, row) =>
                  String(start + (positions.get(row.track_id) ?? 0)),
                );
                if (totals) {
                  editMany(targets, "track_total", (_c, row) =>
                    String(sizes.get(discOf(row)) ?? targets.length),
                  );
                }
              }}
            />
          )}
          {/* Mounted once the files are read: its fields start from the
              table's album and artist. */}
          {tool === "match" && rows.length > 0 && (
            <MatchTool
              key={request?.releaseMbid ?? "match"}
              initialRelease={request?.releaseMbid ?? null}
              album={mostCommon(targets.map((r) => valueOf(r, "album")))}
              artist={
                mostCommon(targets.map((r) => valueOf(r, "album_artist"))) ||
                mostCommon(targets.map((r) => valueOf(r, "artist")))
              }
              taggedRelease={mostCommon(
                targets.map((r) => valueOf(r, "mb_release_id")),
              )}
              targets={targets}
              valueOf={valueOf}
              proposal={proposal}
              onProposal={setProposal}
              onUse={takeProposal}
            />
          )}
        </div>
      )}

      {error && (
        <p role="alert" className="text-sm text-rose-600 dark:text-rose-400">
          {error}
        </p>
      )}
      {report && <ApplyReportBox report={report} />}

      {rows.length === 0 ? (
        <div className="py-16 text-center text-sm text-zinc-500">
          {loading ? (
            <Loader2
              size={18}
              className="inline animate-spin"
              aria-label={t("common.loading")}
            />
          ) : (
            t("tagWorkshop.noFiles")
          )}
        </div>
      ) : (
        <div role="table" aria-label={t("tagWorkshop.title")}>
          <div
            role="row"
            className="sticky top-0 z-10 grid gap-2 items-center px-2 py-2 text-[11px] font-semibold uppercase tracking-wide text-zinc-500 bg-white/95 dark:bg-surface-dark/95 border-b border-zinc-200 dark:border-zinc-800"
            style={{ gridTemplateColumns: gridTemplate }}
          >
            <span role="columnheader">
              <input
                type="checkbox"
                aria-label={t("tagWorkshop.selectAll")}
                checked={allSelected}
                onChange={() =>
                  setSelected(
                    allSelected
                      ? new Set()
                      : new Set(editable.map((row) => row.track_id)),
                  )
                }
              />
            </span>
            <span role="columnheader">{t("tagWorkshop.file")}</span>
            {visibleFields.map((field) => (
              <span key={field} role="columnheader" className="truncate">
                {t(`tagWorkshop.fields.${field}`)}
              </span>
            ))}
          </div>
          <div
            ref={parentRef}
            className="relative w-full"
            style={{ height: `${virtualizer.getTotalSize()}px` }}
          >
            {virtualizer.getVirtualItems().map((item) => {
              const row = rows[item.index];
              return (
                <div
                  key={row.track_id}
                  role="row"
                  className={`absolute left-0 top-0 w-full grid gap-2 items-center px-2 border-b border-zinc-100 dark:border-zinc-800/60 ${
                    row.error ? "opacity-60" : ""
                  }`}
                  style={{
                    height: `${ROW_HEIGHT}px`,
                    transform: `translateY(${item.start - scrollMargin}px)`,
                    gridTemplateColumns: gridTemplate,
                  }}
                >
                  <span role="cell">
                    <input
                      type="checkbox"
                      disabled={row.error != null}
                      aria-label={basename(row.file_path)}
                      checked={selected.has(row.track_id)}
                      onChange={() =>
                        setSelected((prev) => {
                          const next = new Set(prev);
                          if (next.has(row.track_id)) next.delete(row.track_id);
                          else next.add(row.track_id);
                          return next;
                        })
                      }
                    />
                  </span>
                  <span
                    role="cell"
                    className="flex items-center gap-1 min-w-0 text-xs text-zinc-500"
                    title={row.error ?? row.file_path}
                  >
                    {row.error && (
                      <AlertTriangle
                        size={12}
                        className="shrink-0 text-amber-500"
                        aria-hidden="true"
                      />
                    )}
                    <span className="truncate">{basename(row.file_path)}</span>
                  </span>
                  {visibleFields.map((field) => (
                    <input
                      key={field}
                      role="cell"
                      aria-label={`${t(`tagWorkshop.fields.${field}`)} — ${basename(row.file_path)}`}
                      disabled={row.error != null}
                      value={valueOf(row, field)}
                      inputMode={
                        NUMBER_FIELDS.includes(field) ? "numeric" : undefined
                      }
                      spellCheck={false}
                      onChange={(event) =>
                        setCell(row, field, event.target.value)
                      }
                      className={`min-w-0 w-full px-2 py-1 rounded-md text-xs bg-transparent border focus:outline-none focus:ring-2 focus:ring-emerald-500 ${
                        isEdited(row, field)
                          ? "border-amber-400 bg-amber-50 dark:bg-amber-950/30 text-zinc-900 dark:text-zinc-100"
                          : "border-transparent hover:border-zinc-200 dark:hover:border-zinc-700 text-zinc-800 dark:text-zinc-200"
                      } ${IDENTIFIER_FIELDS.includes(field) ? "font-mono text-[11px]" : ""}`}
                    />
                  ))}
                </div>
              );
            })}
          </div>
        </div>
      )}

      <BatchList
        batches={batches}
        busy={writing || workshopTask != null}
        locale={i18n.language}
        onUndo={(id) => void undo(id)}
        undoReport={undoReport}
      />

      {preview && (
        <WorkshopPreviewModal
          previews={preview}
          defaultLabel={
            proposal
              ? `${proposal.release.title} — MusicBrainz`
              : t("tagWorkshop.defaultLabel", { count: preview.length })
          }
          coverOffer={proposal != null}
          onClose={() => setPreview(null)}
          onWrite={(label, withCover) => void write(label, withCover)}
        />
      )}
    </div>
  );
}

const inputClass =
  "px-2.5 py-1.5 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-sm focus:outline-none focus:ring-2 focus:ring-emerald-500";
const actionClass =
  "flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-emerald-500 text-white text-sm font-medium hover:bg-emerald-600 disabled:opacity-50";

function FieldSelect({
  value,
  onChange,
  fields,
  withAll,
}: {
  value: string;
  onChange: (value: string) => void;
  fields: readonly WorkshopField[];
  withAll?: boolean;
}) {
  const { t } = useTranslation();
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className={inputClass}
      aria-label={t("tagWorkshop.field")}
    >
      {withAll && <option value="*">{t("tagWorkshop.allTextFields")}</option>}
      {fields.map((field) => (
        <option key={field} value={field}>
          {t(`tagWorkshop.fields.${field}`)}
        </option>
      ))}
    </select>
  );
}

function FillTool({
  onApply,
}: {
  onApply: (field: WorkshopField, value: string) => void;
}) {
  const { t } = useTranslation();
  const [field, setField] = useState<WorkshopField>("album");
  const [value, setValue] = useState("");
  return (
    <div className="flex flex-wrap items-center gap-2">
      <FieldSelect
        value={field}
        onChange={(v) => setField(v as WorkshopField)}
        fields={WORKSHOP_FIELDS}
      />
      <input
        value={value}
        onChange={(e) => setValue(e.target.value)}
        placeholder={t("tagWorkshop.fillPlaceholder")}
        className={`${inputClass} flex-1 min-w-48`}
      />
      <button
        type="button"
        className={actionClass}
        onClick={() => onApply(field, value)}
      >
        <Check size={14} aria-hidden="true" />
        {t("tagWorkshop.fillApply")}
      </button>
    </div>
  );
}

function ReplaceTool({
  onApply,
}: {
  onApply: (
    fields: readonly WorkshopField[],
    pattern: RegExp,
    spec: { find: string; replace: string; regex: boolean; caseSensitive: boolean },
  ) => void;
}) {
  const { t } = useTranslation();
  const [field, setField] = useState<string>("*");
  const [find, setFind] = useState("");
  const [replace, setReplace] = useState("");
  const [regex, setRegex] = useState(false);
  const [caseSensitive, setCaseSensitive] = useState(false);
  const spec = { find, replace, regex, caseSensitive };
  const compiled = compileFind(spec);
  const compileError = compiled && "error" in compiled ? compiled.error : null;
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-2">
        <FieldSelect
          value={field}
          onChange={setField}
          fields={TEXT_FIELDS}
          withAll
        />
        <input
          value={find}
          onChange={(e) => setFind(e.target.value)}
          placeholder={t("tagWorkshop.find")}
          spellCheck={false}
          className={`${inputClass} flex-1 min-w-40 ${regex ? "font-mono" : ""}`}
        />
        <input
          value={replace}
          onChange={(e) => setReplace(e.target.value)}
          placeholder={t("tagWorkshop.replaceWith")}
          spellCheck={false}
          className={`${inputClass} flex-1 min-w-40`}
        />
        <button
          type="button"
          className={actionClass}
          disabled={!compiled || compileError != null}
          onClick={() => {
            if (!compiled || "error" in compiled) return;
            onApply(
              field === "*" ? TEXT_FIELDS : [field as WorkshopField],
              compiled.pattern,
              spec,
            );
          }}
        >
          <Replace size={14} aria-hidden="true" />
          {t("tagWorkshop.replaceApply")}
        </button>
      </div>
      <div className="flex flex-wrap items-center gap-4 text-sm text-zinc-600 dark:text-zinc-300">
        <label className="flex items-center gap-1.5">
          <input
            type="checkbox"
            checked={regex}
            onChange={() => setRegex(!regex)}
          />
          {t("tagWorkshop.regex")}
        </label>
        <label className="flex items-center gap-1.5">
          <input
            type="checkbox"
            checked={caseSensitive}
            onChange={() => setCaseSensitive(!caseSensitive)}
          />
          {t("tagWorkshop.caseSensitive")}
        </label>
        {compileError && (
          <span role="alert" className="text-xs text-rose-600 dark:text-rose-400">
            {t("tagWorkshop.badRegex", { error: compileError })}
          </span>
        )}
      </div>
    </div>
  );
}

function CaseTool({
  onApply,
}: {
  onApply: (fields: readonly WorkshopField[], mode: CaseMode) => void;
}) {
  const { t } = useTranslation();
  const [field, setField] = useState<string>("title");
  const [mode, setMode] = useState<CaseMode>("title");
  return (
    <div className="flex flex-wrap items-center gap-2">
      <FieldSelect
        value={field}
        onChange={setField}
        fields={TEXT_FIELDS}
        withAll
      />
      <select
        value={mode}
        onChange={(e) => setMode(e.target.value as CaseMode)}
        className={inputClass}
        aria-label={t("tagWorkshop.caseMode")}
      >
        {(["title", "sentence", "upper", "lower"] as CaseMode[]).map((m) => (
          <option key={m} value={m}>
            {t(`tagWorkshop.cases.${m}`)}
          </option>
        ))}
      </select>
      <button
        type="button"
        className={actionClass}
        onClick={() =>
          onApply(field === "*" ? TEXT_FIELDS : [field as WorkshopField], mode)
        }
      >
        <CaseSensitive size={14} aria-hidden="true" />
        {t("tagWorkshop.caseApply")}
      </button>
    </div>
  );
}

function NumberTool({
  onApply,
}: {
  onApply: (start: number, perDisc: boolean, totals: boolean) => void;
}) {
  const { t } = useTranslation();
  const [start, setStart] = useState("1");
  const [perDisc, setPerDisc] = useState(true);
  const [totals, setTotals] = useState(true);
  const first = Math.max(1, Number.parseInt(start, 10) || 1);
  return (
    <div className="flex flex-wrap items-center gap-4">
      <label className="flex items-center gap-2 text-sm text-zinc-600 dark:text-zinc-300">
        {t("tagWorkshop.numberFrom")}
        <input
          value={start}
          inputMode="numeric"
          onChange={(e) => setStart(e.target.value)}
          className={`${inputClass} w-16`}
        />
      </label>
      <label className="flex items-center gap-1.5 text-sm text-zinc-600 dark:text-zinc-300">
        <input
          type="checkbox"
          checked={perDisc}
          onChange={() => setPerDisc(!perDisc)}
        />
        {t("tagWorkshop.numberPerDisc")}
      </label>
      <label className="flex items-center gap-1.5 text-sm text-zinc-600 dark:text-zinc-300">
        <input
          type="checkbox"
          checked={totals}
          onChange={() => setTotals(!totals)}
        />
        {t("tagWorkshop.numberTotals")}
      </label>
      <button
        type="button"
        className={actionClass}
        onClick={() => onApply(first, perDisc, totals)}
      >
        <ListOrdered size={14} aria-hidden="true" />
        {t("tagWorkshop.numberApply")}
      </button>
    </div>
  );
}

function MatchTool({
  initialRelease,
  album: initialAlbum,
  artist: initialArtist,
  taggedRelease,
  targets,
  valueOf,
  proposal,
  onProposal,
  onUse,
}: {
  initialRelease: string | null;
  album: string;
  artist: string;
  taggedRelease: string;
  targets: WorkshopRow[];
  valueOf: (row: WorkshopRow, field: WorkshopField) => string;
  proposal: AlbumProposal | null;
  onProposal: (proposal: AlbumProposal | null) => void;
  onUse: (fields: Set<WorkshopField>) => void;
}) {
  const { t } = useTranslation();
  const [album, setAlbum] = useState(initialAlbum);
  const [artist, setArtist] = useState(initialArtist);
  const [release, setRelease] = useState(initialRelease ?? "");
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [searched, setSearched] = useState(false);
  const [fields, setFields] = useState<Set<WorkshopField>>(
    () => new Set(WORKSHOP_FIELDS.filter((f) => f !== "composer")),
  );
  const autoRan = useRef(false);

  const search = useCallback(async () => {
    setSearching(true);
    setError(null);
    try {
      const number = (row: WorkshopRow, field: WorkshopField) => {
        const n = Number.parseInt(valueOf(row, field), 10);
        return Number.isFinite(n) && n > 0 ? n : null;
      };
      const found = await matchWorkshop(
        album,
        artist || null,
        targets.map((row) => ({
          track_id: row.track_id,
          title: valueOf(row, "title"),
          duration_ms: row.duration_ms > 0 ? row.duration_ms : null,
          track_number: number(row, "track_number"),
          disc_number: number(row, "disc_number"),
        })),
        parseReleaseMbid(release),
        parseReleaseMbid(taggedRelease),
      );
      onProposal(found);
      setSearched(true);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSearching(false);
    }
  }, [album, artist, release, taggedRelease, targets, valueOf, onProposal]);

  // Opened from the enrichment review with the release it proposed:
  // match against it at once, once the files are read.
  useEffect(() => {
    if (!initialRelease || autoRan.current || targets.length === 0) return;
    autoRan.current = true;
    void search();
  }, [initialRelease, targets.length, search]);

  const verdictTone =
    proposal?.verdict === "confident"
      ? "bg-emerald-100 text-emerald-700 dark:bg-emerald-950/60 dark:text-emerald-300"
      : "bg-amber-100 text-amber-700 dark:bg-amber-950/60 dark:text-amber-300";

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <input
          value={album}
          onChange={(e) => setAlbum(e.target.value)}
          placeholder={t("tagWorkshop.fields.album")}
          className={`${inputClass} flex-1 min-w-40`}
        />
        <input
          value={artist}
          onChange={(e) => setArtist(e.target.value)}
          placeholder={t("tagWorkshop.fields.album_artist")}
          className={`${inputClass} flex-1 min-w-40`}
        />
        <input
          value={release}
          onChange={(e) => setRelease(e.target.value)}
          placeholder={t("tagWorkshop.releasePlaceholder")}
          spellCheck={false}
          className={`${inputClass} flex-1 min-w-56 font-mono text-xs`}
        />
        <button
          type="button"
          className={actionClass}
          disabled={searching || targets.length === 0 || (!album && !release)}
          onClick={() => void search()}
        >
          {searching ? (
            <Loader2 size={14} className="animate-spin" aria-hidden="true" />
          ) : (
            <Wand2 size={14} aria-hidden="true" />
          )}
          {t("tagWorkshop.matchSearch")}
        </button>
      </div>
      <p className="text-xs text-zinc-500">{t("tagWorkshop.matchHint")}</p>
      {error && (
        <p role="alert" className="text-xs text-rose-600 dark:text-rose-400">
          {error}
        </p>
      )}
      {searched && !proposal && !error && (
        <p className="text-sm text-zinc-500">{t("tagWorkshop.matchNone")}</p>
      )}
      {proposal && (
        <div className="rounded-xl border border-zinc-200 dark:border-zinc-800 p-3 space-y-3">
          <div className="flex flex-wrap items-start justify-between gap-2">
            <div className="min-w-0">
              <div className="truncate text-sm font-semibold">
                {proposal.release.title}
              </div>
              <div className="truncate text-xs text-zinc-500">
                {[
                  proposal.release.artist,
                  proposal.release.date,
                  proposal.release.country,
                  t("tagWorkshop.releaseTracks", {
                    count: proposal.release.track_count,
                  }),
                ]
                  .filter(Boolean)
                  .join(" · ")}
              </div>
              <button
                type="button"
                onClick={() =>
                  void openUrl(
                    musicBrainzUrl("release", proposal.release.mbid),
                  ).catch((err) =>
                    console.warn("[TagWorkshop] open failed", err),
                  )
                }
                className="mt-1 text-[11px] text-zinc-500 hover:text-emerald-600 dark:hover:text-emerald-400"
              >
                {t("enrichmentReview.openMusicBrainz")}
              </button>
            </div>
            <span
              className={`shrink-0 px-2 py-0.5 rounded-full text-xs font-medium ${verdictTone}`}
            >
              {t(`tagWorkshop.verdicts.${proposal.verdict}`, {
                score: Math.round(proposal.score.total * 100),
              })}
            </span>
          </div>
          <p className="text-xs text-zinc-500">
            {t("tagWorkshop.matchPaired", {
              paired: proposal.tracks.length,
              count: targets.length,
            })}
            {proposal.from_tags && ` · ${t("tagWorkshop.matchFromTags")}`}
          </p>
          <div className="flex flex-wrap gap-x-4 gap-y-1.5">
            {WORKSHOP_FIELDS.filter((f) => f !== "composer").map((field) => (
              <label
                key={field}
                className="flex items-center gap-1.5 text-xs text-zinc-600 dark:text-zinc-300"
              >
                <input
                  type="checkbox"
                  checked={fields.has(field)}
                  onChange={() =>
                    setFields((prev) => {
                      const next = new Set(prev);
                      if (next.has(field)) next.delete(field);
                      else next.add(field);
                      return next;
                    })
                  }
                />
                {t(`tagWorkshop.fields.${field}`)}
              </label>
            ))}
          </div>
          <button
            type="button"
            className={actionClass}
            disabled={fields.size === 0 || proposal.tracks.length === 0}
            onClick={() => onUse(fields)}
          >
            <Check size={14} aria-hidden="true" />
            {t("tagWorkshop.matchUse")}
          </button>
        </div>
      )}
    </div>
  );
}

function ApplyReportBox({ report }: { report: ApplyReport }) {
  const { t } = useTranslation();
  return (
    <div className="rounded-2xl border border-zinc-200 dark:border-zinc-800 p-3 text-sm space-y-1">
      <p>
        {t("tagWorkshop.reportWritten", { count: report.written })}
        {report.unchanged > 0 &&
          ` · ${t("tagWorkshop.reportUnchanged", { count: report.unchanged })}`}
        {report.covers > 0 &&
          ` · ${t("tagWorkshop.reportCovers", { count: report.covers })}`}
        {report.cancelled && ` · ${t("tagWorkshop.reportCancelled")}`}
      </p>
      {report.problems.length > 0 && <ProblemList problems={report.problems} />}
    </div>
  );
}

function ProblemList({
  problems,
}: {
  problems: { file_path: string; reason: string }[];
}) {
  const { t } = useTranslation();
  const known = ["changed_since", "missing", "interrupted", "invalid"];
  return (
    <ul className="space-y-0.5 text-xs text-rose-600 dark:text-rose-400">
      {problems.slice(0, 50).map((p, i) => (
        <li key={`${p.file_path}-${i}`} className="truncate" title={p.file_path}>
          {basename(p.file_path) || "—"} —{" "}
          {known.includes(p.reason)
            ? t(`tagWorkshop.problems.${p.reason}`)
            : p.reason}
        </li>
      ))}
      {problems.length > 50 && (
        <li>{t("tagWorkshop.moreProblems", { count: problems.length - 50 })}</li>
      )}
    </ul>
  );
}

function BatchList({
  batches,
  busy,
  locale,
  onUndo,
  undoReport,
}: {
  batches: BatchSummary[];
  busy: boolean;
  locale: string;
  onUndo: (batchId: number) => void;
  undoReport: UndoReport | null;
}) {
  const { t } = useTranslation();
  const [confirming, setConfirming] = useState<number | null>(null);
  const format = useMemo(
    () =>
      new Intl.DateTimeFormat(locale, {
        dateStyle: "medium",
        timeStyle: "short",
      }),
    [locale],
  );
  return (
    <section className="space-y-2 pt-4">
      <h2 className="text-sm font-semibold text-zinc-700 dark:text-zinc-200">
        {t("tagWorkshop.batches")}
      </h2>
      <p className="text-xs text-zinc-500">{t("tagWorkshop.batchesHint")}</p>
      {undoReport && (
        <div className="rounded-xl border border-zinc-200 dark:border-zinc-800 p-3 text-sm space-y-1">
          <p>
            {t("tagWorkshop.undoRestored", { count: undoReport.restored })}
            {undoReport.cancelled && ` · ${t("tagWorkshop.reportCancelled")}`}
          </p>
          {undoReport.refused.length > 0 && (
            <ProblemList problems={undoReport.refused} />
          )}
        </div>
      )}
      {batches.length === 0 ? (
        <p className="text-sm text-zinc-500">{t("tagWorkshop.noBatches")}</p>
      ) : (
        <ul className="divide-y divide-zinc-100 dark:divide-zinc-800">
          {batches.map((batch) => {
            const undone = batch.undone_at != null;
            const open = batch.written + batch.pending;
            return (
              <li
                key={batch.id}
                className="flex flex-wrap items-center justify-between gap-2 py-2"
              >
                <div className="min-w-0">
                  <div className="truncate text-sm">{batch.label || "—"}</div>
                  <div className="text-xs text-zinc-500">
                    {format.format(new Date(batch.created_at))} ·{" "}
                    {t("tagWorkshop.batchFiles", { count: batch.files })}
                    {batch.undone > 0 &&
                      ` · ${t("tagWorkshop.batchUndone", { count: batch.undone })}`}
                    {batch.failed > 0 &&
                      ` · ${t("tagWorkshop.batchFailed", { count: batch.failed })}`}
                  </div>
                </div>
                {undone ? (
                  <span className="text-xs text-zinc-500">
                    {t("tagWorkshop.batchIsUndone")}
                  </span>
                ) : (
                  open > 0 && (
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => {
                        if (confirming === batch.id) {
                          setConfirming(null);
                          onUndo(batch.id);
                        } else {
                          setConfirming(batch.id);
                        }
                      }}
                      onBlur={() =>
                        setConfirming((c) => (c === batch.id ? null : c))
                      }
                      className={`flex items-center gap-1 px-2.5 py-1 rounded-lg text-xs font-medium disabled:opacity-50 ${
                        confirming === batch.id
                          ? "bg-rose-500 text-white hover:bg-rose-600"
                          : "border border-zinc-200 dark:border-zinc-700 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-800"
                      }`}
                    >
                      <Undo2 size={12} aria-hidden="true" />
                      {confirming === batch.id
                        ? t("tagWorkshop.undoConfirm")
                        : t("tagWorkshop.undo")}
                    </button>
                  )
                )}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

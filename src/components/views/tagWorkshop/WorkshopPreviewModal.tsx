import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useVirtualizer } from "@tanstack/react-virtual";
import { AlertTriangle, ArrowRight, Eye, PenLine, X } from "lucide-react";

import { useModalA11y } from "../../../hooks/useModalA11y";
import type { FilePreview } from "../../../lib/tauri/tagWorkshop";
import {
  AnimatedModalContent,
  AnimatedModalShell,
} from "../../common/AnimatedModalShell";

/** Estimated height of one file's card; cards are measured once drawn. */
const CARD_ESTIMATE = 96;

function basename(path: string): string {
  const cut = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return cut >= 0 ? path.slice(cut + 1) : path;
}

/**
 * What a batch would write, file by file and field by field — what each
 * file says now beside what it would say — before anything is written.
 *
 * The values come from the backend reading every file again, so they are
 * the files', not the table's. "Write" is the one control that writes;
 * a file the preview flags (a value that cannot be written, a file that
 * cannot be read) blocks it until the table is fixed.
 *
 * Virtualized against the dialog's own scroller: a folder's preview can
 * list thousands of files.
 */
export function WorkshopPreviewModal({
  previews,
  defaultLabel,
  coverOffer,
  onClose,
  onWrite,
}: {
  previews: FilePreview[];
  defaultLabel: string;
  /** A MusicBrainz release is behind the change: offer its cover. */
  coverOffer: boolean;
  onClose: () => void;
  onWrite: (label: string, withCover: boolean) => void;
}) {
  const { t } = useTranslation();
  const dialogRef = useModalA11y<HTMLDivElement>(true, onClose);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const [label, setLabel] = useState(defaultLabel);
  const [withCover, setWithCover] = useState(coverOffer);

  const writable = previews.filter(
    (p) => p.diffs.length > 0 && !p.invalid && !p.error,
  );
  const blocked = previews.filter((p) => p.invalid || p.error);
  const fieldCount = writable.reduce((sum, p) => sum + p.diffs.length, 0);

  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: previews.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => CARD_ESTIMATE,
    overscan: 6,
  });

  return (
    <AnimatedModalShell isOpen onBackdropClick={onClose}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="tag-workshop-preview-title"
        className="relative bg-white dark:bg-surface-dark-elevated text-zinc-900 dark:text-zinc-100 rounded-3xl border border-zinc-200 dark:border-zinc-800 shadow-2xl w-full max-w-4xl h-[85vh] flex flex-col overflow-hidden"
      >
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-200 dark:border-zinc-800">
          <div className="flex items-center gap-2">
            <Eye size={18} className="text-emerald-500" aria-hidden="true" />
            <h2
              id="tag-workshop-preview-title"
              className="text-lg font-semibold"
            >
              {t("tagWorkshop.previewTitle")}
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

        <p className="px-6 pt-3 text-sm text-zinc-600 dark:text-zinc-300">
          {previews.length === 0
            ? t("tagWorkshop.previewNothing")
            : t("tagWorkshop.previewSummary", {
                count: writable.length,
                fields: fieldCount,
              })}
        </p>
        {blocked.length > 0 && (
          <p
            role="alert"
            className="px-6 pt-1 text-xs text-rose-600 dark:text-rose-400"
          >
            {t("tagWorkshop.previewBlocked", { count: blocked.length })}
          </p>
        )}

        <div
          ref={scrollRef}
          className="flex-1 overflow-y-auto px-6 py-3"
        >
          <div
            className="relative w-full"
            style={{ height: `${virtualizer.getTotalSize()}px` }}
          >
            {virtualizer.getVirtualItems().map((item) => {
              const file = previews[item.index];
              return (
                <div
                  key={item.key}
                  data-index={item.index}
                  ref={virtualizer.measureElement}
                  className="absolute left-0 top-0 w-full pb-2"
                  style={{ transform: `translateY(${item.start}px)` }}
                >
                  <div className="rounded-2xl border border-zinc-200 dark:border-zinc-800 p-3">
                    <div
                      className="truncate text-sm font-medium"
                      title={file.file_path}
                    >
                      {basename(file.file_path) || "—"}
                    </div>
                    {(file.error || file.invalid) && (
                      <div className="mt-1 flex items-center gap-1 text-xs text-rose-600 dark:text-rose-400">
                        <AlertTriangle size={12} aria-hidden="true" />
                        {file.error ??
                          (file.invalid &&
                            t(`tagWorkshop.invalid.${file.invalid.reason}`, {
                              field: t(
                                `tagWorkshop.fields.${file.invalid.field}`,
                              ),
                            }))}
                      </div>
                    )}
                    <dl className="mt-2 grid grid-cols-[minmax(7rem,auto)_1fr] gap-x-3 gap-y-1 text-xs">
                      {file.diffs.map((d) => (
                        <div key={d.field} className="contents">
                          <dt className="text-zinc-500">
                            {t(`tagWorkshop.fields.${d.field}`)}
                          </dt>
                          <dd className="flex flex-wrap items-center gap-1.5 min-w-0">
                            <span
                              className={`break-all ${
                                d.old
                                  ? "text-rose-700 dark:text-rose-300 line-through decoration-rose-400/60"
                                  : "italic text-zinc-400"
                              }`}
                            >
                              {d.old || t("tagWorkshop.empty")}
                            </span>
                            <ArrowRight
                              size={11}
                              className="shrink-0 text-zinc-400"
                              aria-label={t("tagWorkshop.becomes")}
                            />
                            <span
                              className={`break-all ${
                                d.new
                                  ? "text-emerald-700 dark:text-emerald-300"
                                  : "italic text-zinc-400"
                              }`}
                            >
                              {d.new || t("tagWorkshop.removed")}
                            </span>
                          </dd>
                        </div>
                      ))}
                    </dl>
                  </div>
                </div>
              );
            })}
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-3 px-6 py-4 border-t border-zinc-200 dark:border-zinc-800">
          <label className="flex flex-1 min-w-56 items-center gap-2 text-sm text-zinc-600 dark:text-zinc-300">
            {t("tagWorkshop.batchLabel")}
            <input
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              className="flex-1 min-w-0 px-2.5 py-1.5 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-sm focus:outline-none focus:ring-2 focus:ring-emerald-500"
            />
          </label>
          {coverOffer && (
            <label className="flex items-center gap-1.5 text-sm text-zinc-600 dark:text-zinc-300">
              <input
                type="checkbox"
                checked={withCover}
                onChange={() => setWithCover(!withCover)}
              />
              {t("tagWorkshop.coverFill")}
            </label>
          )}
          <button
            type="button"
            onClick={onClose}
            className="px-3 py-2 rounded-xl text-sm text-zinc-600 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800"
          >
            {t("common.cancel")}
          </button>
          <button
            type="button"
            disabled={writable.length === 0 || blocked.length > 0}
            onClick={() => onWrite(label.trim(), coverOffer && withCover)}
            className="flex items-center gap-1.5 px-4 py-2 rounded-xl bg-emerald-500 text-white text-sm font-medium hover:bg-emerald-600 disabled:opacity-50"
          >
            <PenLine size={14} aria-hidden="true" />
            {t("tagWorkshop.write", { count: writable.length })}
          </button>
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}

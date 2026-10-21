import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, CheckCircle2, X } from "lucide-react";

import {
  IMPORT_NOTICE_EVENT,
  type ImportNotice,
} from "../../lib/tauri/library";

/** Long enough to read a few lines, for a notice that needs nothing. */
const AUTO_HIDE_MS = 6_000;

interface Shown {
  notice: ImportNotice;
  /** Distinguishes one notice from the next, so a repeat re-shows. */
  key: number;
}

/**
 * What adding to the library did, where the scan's own card cannot say:
 * a folder refused for overlapping one already in the library, and what a
 * drop copied into the managed folder and what it passed over. Fed by
 * `waveflow:import-notice` ({@link announceImport}), so the four places
 * that add a folder and the drop are covered without each knowing.
 *
 * Bottom centre, apart from the scan's card at the bottom right: a drop
 * shows both, the copy here and the scan's outcome there. A notice that
 * needs looking at (a refusal, a failure, a cut) stays until dismissed.
 */
export function ImportNoticeToast() {
  const { t } = useTranslation();
  const [shown, setShown] = useState<Shown | null>(null);
  const timer = useRef<number | null>(null);

  useEffect(() => {
    let next = 0;
    const onNotice = (event: Event) => {
      const notice = (event as CustomEvent<ImportNotice>).detail;
      if (!notice) return;
      next += 1;
      setShown({ notice, key: next });
      if (timer.current != null) window.clearTimeout(timer.current);
      timer.current = null;
      if (!needsAttention(notice)) {
        timer.current = window.setTimeout(() => setShown(null), AUTO_HIDE_MS);
      }
    };
    window.addEventListener(IMPORT_NOTICE_EVENT, onNotice);
    return () => {
      window.removeEventListener(IMPORT_NOTICE_EVENT, onNotice);
      if (timer.current != null) window.clearTimeout(timer.current);
    };
  }, []);

  if (!shown) return null;
  const { notice } = shown;
  const attention = needsAttention(notice);

  let title: string;
  let lines: string[];
  let folder: string | null = null;
  if (notice.kind === "overlap") {
    title = t("importNotice.overlap.title");
    lines = [
      t(`importNotice.overlap.${notice.overlap.relation}`, {
        folder: notice.overlap.folder,
      }),
    ];
  } else {
    const s = notice.summary;
    // Files dropped again after their folder left the library are not
    // copied, but the scan puts them back: that is still an addition.
    title =
      s.copied > 0 || (s.scan?.added ?? 0) > 0
        ? t("importNotice.dropped.title")
        : t("importNotice.dropped.titleNothing");
    folder = s.copied > 0 || s.already_copied > 0 ? s.folder : null;
    lines = [
      s.copied > 0 && t("importNotice.dropped.copied", { n: s.copied }),
      s.already_copied > 0 &&
        t("importNotice.dropped.alreadyCopied", { n: s.already_copied }),
      s.in_library > 0 &&
        t("importNotice.dropped.inLibrary", { n: s.in_library }),
      s.refused > 0 && t("importNotice.dropped.refused", { n: s.refused }),
      s.errors > 0 && t("importNotice.dropped.errors", { n: s.errors }),
      s.truncated && t("importNotice.dropped.truncated"),
      s.cancelled && t("importNotice.dropped.cancelled"),
    ].filter((line): line is string => typeof line === "string");
  }

  return (
    <div
      key={shown.key}
      role="status"
      aria-live="polite"
      data-import-notice
      className="fixed bottom-28 left-1/2 -translate-x-1/2 z-50 w-96 max-w-[calc(100vw-2rem)] rounded-2xl border border-zinc-200 dark:border-zinc-700 bg-white/95 dark:bg-zinc-900/95 backdrop-blur shadow-xl p-4 animate-fade-in"
    >
      <div className="flex items-start gap-3">
        <div
          className={`shrink-0 w-9 h-9 rounded-full flex items-center justify-center ${
            attention
              ? "bg-amber-500/15 text-amber-500"
              : "bg-emerald-500/15 text-emerald-500"
          }`}
        >
          {attention ? <AlertTriangle size={18} /> : <CheckCircle2 size={18} />}
        </div>
        <div className="flex-1 min-w-0">
          <div
            className={`text-sm font-semibold ${
              attention
                ? "text-amber-700 dark:text-amber-500"
                : "text-zinc-900 dark:text-zinc-100"
            }`}
          >
            {title}
          </div>
          {folder && (
            <div
              className="text-[11px] text-zinc-400 dark:text-zinc-500 truncate"
              title={folder}
            >
              {folder}
            </div>
          )}
          {lines.map((line) => (
            <div
              key={line}
              className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5"
            >
              {line}
            </div>
          ))}
        </div>
        <button
          type="button"
          onClick={() => setShown(null)}
          aria-label={t("common.close")}
          className="shrink-0 p-1 rounded hover:bg-zinc-100 dark:hover:bg-zinc-800 text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors"
        >
          <X size={14} />
        </button>
      </div>
    </div>
  );
}

/** A refusal, a failure, a cut or a stop: kept on screen until read. */
function needsAttention(notice: ImportNotice): boolean {
  if (notice.kind === "overlap") return true;
  const s = notice.summary;
  return (
    s.refused > 0 ||
    s.errors > 0 ||
    s.truncated ||
    s.cancelled ||
    (s.copied === 0 && s.already_copied === 0)
  );
}

import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Undo2 } from "lucide-react";

import { useModalA11y } from "../../../hooks/useModalA11y";
import {
  AnimatedModalContent,
  AnimatedModalShell,
} from "../../common/AnimatedModalShell";
import { Artwork } from "../../common/Artwork";
import {
  MOODS,
  listMoodOverrides,
  setMoodOverride,
  type Mood,
  type MoodOverrideKind,
  type MoodOverrideTrack,
} from "../../../lib/tauri/moodRadio";

interface MoodCurationModalProps {
  isOpen: boolean;
  onClose: () => void;
  /** A change was undone: the tile counts are stale. */
  onChanged: () => void;
}

/**
 * The tracks put into or taken out of each mood by hand, one tab per
 * mood, each undoable. Changes are made from a track's menu (Moods);
 * this is where they are reviewed, without building a playlist.
 */
export function MoodCurationModal({
  isOpen,
  onClose,
  onChanged,
}: MoodCurationModalProps) {
  const { t } = useTranslation();
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);
  const [mood, setMood] = useState<Mood>("focus");
  const [rows, setRows] = useState<MoodOverrideTrack[] | null>(null);
  const [reload, setReload] = useState(0);

  useEffect(() => {
    if (!isOpen) return;
    // Not the last opening's list: a track menu may have changed it since.
    setRows(null);
    let cancelled = false;
    listMoodOverrides(mood)
      .then((r) => {
        if (!cancelled) setRows(r);
      })
      .catch((err) =>
        console.error("[MoodCurationModal] list overrides failed", err),
      );
    return () => {
      cancelled = true;
    };
  }, [isOpen, mood, reload]);

  const selectMood = (next: Mood) => {
    if (next === mood) return;
    setRows(null);
    setMood(next);
  };

  const undo = (trackId: number) => {
    setMoodOverride(mood, [trackId], null)
      .then(() => {
        setReload((n) => n + 1);
        onChanged();
      })
      .catch((err) => console.error("[MoodCurationModal] undo failed", err));
  };

  const section = (kind: MoodOverrideKind) => {
    const items = (rows ?? []).filter((r) => r.kind === kind);
    if (items.length === 0) return null;
    return (
      <section>
        <h3 className="px-1 mb-1 text-[11px] font-semibold uppercase tracking-wide text-zinc-400">
          {t(
            kind === "include" ? "moodCuration.added" : "moodCuration.removed",
          )}
        </h3>
        <ul className="flex flex-col gap-1">
          {items.map(({ track }) => (
            <li
              key={track.id}
              className="flex items-center gap-3 rounded-lg p-1.5 hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
            >
              <Artwork
                path={track.artwork_path}
                path1x={track.artwork_path_1x}
                path2x={track.artwork_path_2x}
                size="1x"
                className="w-9 h-9 shrink-0"
                iconSize={16}
                alt={track.album_title ?? track.title}
                rounded="md"
              />
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm text-zinc-800 dark:text-zinc-100">
                  {track.title}
                </div>
                <div className="truncate text-xs text-zinc-500 dark:text-zinc-400">
                  {track.artist_name}
                </div>
              </div>
              <button
                type="button"
                onClick={() => undo(track.id)}
                title={t("moodCuration.undo")}
                aria-label={t("moodCuration.undoTrack", {
                  title: track.title,
                })}
                className="shrink-0 rounded-lg p-2 text-zinc-400 hover:bg-zinc-100 hover:text-zinc-700 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 dark:hover:bg-zinc-800 dark:hover:text-zinc-200"
              >
                <Undo2 size={16} />
              </button>
            </li>
          ))}
        </ul>
      </section>
    );
  };

  return (
    <AnimatedModalShell isOpen={isOpen} onBackdropClick={onClose}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="mood-curation-title"
        className="relative flex max-h-[80vh] w-full max-w-lg flex-col rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated"
      >
        <h2
          id="mood-curation-title"
          className="mb-1 text-lg font-bold text-zinc-900 dark:text-white"
        >
          {t("moodCuration.title")}
        </h2>
        <p className="mb-4 text-xs text-zinc-500 dark:text-zinc-400">
          {t("moodCuration.subtitle")}
        </p>

        <div role="tablist" className="mb-4 flex flex-wrap gap-2">
          {MOODS.map((m) => (
            <button
              key={m}
              type="button"
              role="tab"
              aria-selected={m === mood}
              onClick={() => selectMood(m)}
              className={`rounded-full px-3 py-1.5 text-xs font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${
                m === mood
                  ? "bg-emerald-500 text-white"
                  : "bg-zinc-100 text-zinc-600 hover:bg-zinc-200 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700"
              }`}
            >
              {t(`home.moodRadio.${m}.title`)}
            </button>
          ))}
        </div>

        {/* A short list in a dialog: `overscroll-contain` keeps the
            page behind it from scrolling once the end is reached. */}
        <div className="-mx-1 min-h-32 flex-1 overflow-y-auto overscroll-contain px-1">
          {rows == null ? (
            <p className="py-8 text-center text-sm text-zinc-400">
              {t("common.loading")}
            </p>
          ) : rows.length === 0 ? (
            <p className="py-8 text-center text-sm text-zinc-500 dark:text-zinc-400">
              {t("moodCuration.empty")}
            </p>
          ) : (
            <div className="flex flex-col gap-4">
              {section("include")}
              {section("exclude")}
            </div>
          )}
        </div>

        <div className="mt-4 flex justify-end border-t border-zinc-100 pt-3 dark:border-zinc-800">
          <button
            type="button"
            onClick={onClose}
            className="rounded-xl px-4 py-2 text-sm font-medium text-zinc-500 transition-colors hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-zinc-200"
          >
            {t("common.close")}
          </button>
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}

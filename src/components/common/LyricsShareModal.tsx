import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Copy, Download, Loader2, Quote } from "lucide-react";

import { useModalA11y } from "../../hooks/useModalA11y";
import { useShareImage } from "../../hooks/useShareImage";
import { MAX_LYRIC_LINES, renderLyricsCard } from "../../lib/lyricsCard";
import { resolveRemoteImage } from "../../lib/tauri/artwork";
import type { Track } from "../../lib/tauri/track";
import { AnimatedModalContent, AnimatedModalShell } from "./AnimatedModalShell";

interface LyricsShareModalProps {
  isOpen: boolean;
  onClose: () => void;
  track: Track;
  /** The lyric's lines as they read, in song order. */
  lines: string[];
  /** The line playing when the dialog opened, selected to start with
   *  (`-1` for none). */
  initialIndex: number;
}

/** How long the selection rests before the preview is redrawn: a card
 *  takes a few frames to render, and a click on each of four lines
 *  needs one preview, not four. */
const PREVIEW_DELAY_MS = 150;

/**
 * Pick a few lines of the playing track's lyrics and save or copy them
 * as an image with the cover ({@link renderLyricsCard}). Opened from the
 * lyrics panel and from the immersive view's share menu; the save and
 * copy flow is `useShareImage`, shared with every other share card.
 */
export function LyricsShareModal({
  isOpen,
  onClose,
  track,
  lines,
  initialIndex,
}: LyricsShareModalProps) {
  const { t } = useTranslation();
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);
  // `useLyricsShare` keys the dialog per opening, so the initial
  // selection is read once each time it opens.
  const [selected, setSelected] = useState<number[]>(() =>
    initialIndex >= 0 &&
    initialIndex < lines.length &&
    lines[initialIndex].trim()
      ? [initialIndex]
      : [],
  );
  const full = selected.length >= MAX_LYRIC_LINES;

  const excerpt = useMemo(
    () => selected.map((i) => lines[i]).filter((line) => line != null),
    [selected, lines],
  );
  const coverSrc = resolveRemoteImage(
    track.artwork_path ?? track.artwork_path_2x ?? track.artwork_path_1x,
    null,
  );
  const labels = {
    eyebrow: t("lyrics.share.eyebrow"),
    footer: t("nowPlaying.share.footer", { app: "WaveFlow" }),
  };
  const build = () =>
    renderLyricsCard({
      lines: excerpt,
      title: track.title,
      artist: track.artist_name ?? null,
      coverSrc,
      labels,
    });
  const share = useShareImage(build, track.title);

  // ----- Preview --------------------------------------------------------
  const [preview, setPreview] = useState<string | null>(null);
  const previewRef = useRef<string | null>(null);
  const buildRef = useRef(build);
  useEffect(() => {
    buildRef.current = build;
  });
  useEffect(() => {
    if (!isOpen || excerpt.length === 0) return;
    let cancelled = false;
    const timer = window.setTimeout(() => {
      buildRef.current().then(
        (blob) => {
          if (cancelled) return;
          const url = URL.createObjectURL(blob);
          if (previewRef.current) URL.revokeObjectURL(previewRef.current);
          previewRef.current = url;
          setPreview(url);
        },
        (err: unknown) => {
          console.error("[LyricsShareModal] preview failed", err);
        },
      );
    }, PREVIEW_DELAY_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [isOpen, excerpt]);
  // The last preview's URL is released with the dialog.
  useEffect(
    () => () => {
      if (previewRef.current) URL.revokeObjectURL(previewRef.current);
    },
    [],
  );

  const toggle = (index: number) => {
    setSelected((current) => {
      if (current.includes(index)) return current.filter((i) => i !== index);
      if (current.length >= MAX_LYRIC_LINES) return current;
      return [...current, index].sort((a, b) => a - b);
    });
  };

  return (
    <AnimatedModalShell isOpen={isOpen} onBackdropClick={onClose}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="lyrics-share-title"
        className="relative flex max-h-[90dvh] w-full max-w-3xl flex-col rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated"
      >
        <h2
          id="lyrics-share-title"
          className="text-lg font-bold text-zinc-900 dark:text-white"
        >
          {t("lyrics.share.title")}
        </h2>
        <p
          className="mt-1 text-xs text-zinc-500 dark:text-zinc-400"
          role="status"
        >
          {t("lyrics.share.hint", { max: MAX_LYRIC_LINES })}{" "}
          <span className="tabular-nums">
            {t("lyrics.share.counter", {
              selected: selected.length,
              max: MAX_LYRIC_LINES,
            })}
          </span>
        </p>

        <div className="mt-4 grid min-h-0 flex-1 gap-5 md:grid-cols-[minmax(0,18rem)_minmax(0,1fr)]">
          <div className="flex flex-col items-center gap-3">
            <div className="aspect-square w-full max-w-72 overflow-hidden rounded-2xl bg-zinc-100 dark:bg-zinc-800">
              {excerpt.length > 0 && preview ? (
                <img
                  src={preview}
                  alt={t("lyrics.share.previewAlt")}
                  className="h-full w-full object-cover"
                />
              ) : (
                <div className="flex h-full w-full flex-col items-center justify-center gap-2 p-6 text-center text-xs text-zinc-500 dark:text-zinc-400">
                  <Quote size={28} aria-hidden="true" className="opacity-60" />
                  {t("lyrics.share.empty")}
                </div>
              )}
            </div>
            <div className="flex w-full max-w-72 gap-2">
              <button
                type="button"
                onClick={() => void share.save()}
                disabled={excerpt.length === 0 || share.busy}
                className="flex flex-1 items-center justify-center gap-2 rounded-xl bg-emerald-500 px-3 py-2 text-sm font-semibold text-white transition-colors hover:bg-emerald-600 disabled:opacity-50"
              >
                {share.state === "saving" ? (
                  <Loader2 size={15} className="motion-safe:animate-spin" />
                ) : share.state === "done" ? (
                  <Check size={15} />
                ) : (
                  <Download size={15} />
                )}
                {t("nowPlaying.share.save")}
              </button>
              <button
                type="button"
                onClick={() => void share.copy()}
                disabled={excerpt.length === 0 || share.busy}
                aria-label={t("nowPlaying.share.copy")}
                title={t("nowPlaying.share.copy")}
                className="flex items-center justify-center rounded-xl border border-zinc-200 px-3 py-2 text-zinc-600 transition-colors hover:bg-zinc-100 disabled:opacity-50 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800"
              >
                {share.state === "copying" ? (
                  <Loader2 size={15} className="motion-safe:animate-spin" />
                ) : (
                  <Copy size={15} />
                )}
              </button>
            </div>
          </div>

          <ul
            aria-label={t("lyrics.share.linesLabel")}
            className="min-h-0 space-y-0.5 overflow-y-auto rounded-2xl border border-zinc-200 p-2 dark:border-zinc-800 max-md:max-h-[40dvh]"
          >
            {lines.map((line, index) =>
              line.trim() ? (
                <li key={index}>
                  <button
                    type="button"
                    dir="auto"
                    aria-pressed={selected.includes(index)}
                    disabled={full && !selected.includes(index)}
                    onClick={() => toggle(index)}
                    className={`w-full rounded-lg px-3 py-1.5 text-start text-sm wrap-anywhere transition-colors disabled:cursor-not-allowed disabled:opacity-40 ${
                      selected.includes(index)
                        ? "bg-emerald-500/15 font-medium text-emerald-700 dark:text-emerald-300"
                        : "text-zinc-700 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
                    }`}
                  >
                    {line}
                  </button>
                </li>
              ) : null,
            )}
          </ul>
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

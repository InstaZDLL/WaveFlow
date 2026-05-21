import { useEffect, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import { X, Music2 } from "lucide-react";
import { Artwork } from "../common/Artwork";
import type { Track } from "../../lib/tauri/track";
import {
  findActiveWordIndex,
  type LyricsLine,
  type LyricsPayload,
} from "../../lib/tauri/lyrics";
import { usePlayer } from "../../hooks/usePlayer";
import { useModalA11y } from "../../hooks/useModalA11y";

interface FullscreenLyricsProps {
  track: Track;
  payload: LyricsPayload | null;
  lrcLines: LyricsLine[];
  isSynced: boolean;
  activeIndex: number;
  isFetching: boolean;
  error: string | null;
  onClose: () => void;
  /** Clicking the header cover switches back to the immersive Now
   *  Playing overlay. The parent flips the fullscreen mutex so this
   *  view unmounts and FullscreenNowPlaying paints in its place. */
  onOpenNowPlaying: () => void;
  onSeek: (line: LyricsLine) => void;
}

/**
 * Apple-Music-style karaoke fullscreen overlay. Receives the already-
 * fetched lyrics payload from `LyricsPanel` so we don't double-fetch
 * (the side panel stays mounted underneath). Active line is centered
 * with smooth scroll, neighbours fade out as they move away from the
 * focal point, and a blurred copy of the artwork doubles as the
 * background so the view stays visually anchored to the track.
 *
 * Click any line to seek; Escape or the close button dismisses.
 */
export function FullscreenLyrics({
  track,
  payload,
  lrcLines,
  isSynced,
  activeIndex,
  isFetching,
  error,
  onClose,
  onOpenNowPlaying,
  onSeek,
}: FullscreenLyricsProps) {
  const { t } = useTranslation();
  const { positionMs } = usePlayer();
  const lineRefs = useRef<Array<HTMLLIElement | null>>([]);
  // The overlay is mounted only when the side panel toggles it on, so
  // the hook is always opened against `true` while alive — passing
  // `true` here keeps the focus trap, Escape-close, and focus
  // restoration consistent with the rest of the modal stack.
  const dialogRef = useModalA11y<HTMLDivElement>(true, onClose);

  // Active word index inside the current line — drives the per-word
  // karaoke highlight. Recomputed on every position tick but only when
  // the current line actually carries word stamps.
  const activeLine = activeIndex >= 0 ? lrcLines[activeIndex] : undefined;
  const activeWordIndex = useMemo(() => {
    if (!activeLine?.words || activeLine.words.length === 0) return -1;
    return findActiveWordIndex(activeLine.words, positionMs);
  }, [activeLine, positionMs]);

  // Keep the active line vertically centered. Independent ref array
  // from the side panel so both views can scroll in parallel.
  useEffect(() => {
    if (!isSynced || activeIndex < 0) return;
    const node = lineRefs.current[activeIndex];
    if (node) {
      node.scrollIntoView({ behavior: "smooth", block: "center" });
    }
  }, [activeIndex, isSynced]);

  return (
    <div
      ref={dialogRef}
      role="dialog"
      aria-modal="true"
      aria-labelledby="fullscreen-lyrics-title"
      className="fixed inset-0 z-[100] animate-fade-in"
    >
      {/* Blurred artwork background — falls back to a flat dark
          gradient when the track has no cover. */}
      <div className="absolute inset-0 overflow-hidden">
        {track.artwork_path ? (
          <Artwork
            path={track.artwork_path}
            path1x={track.artwork_path_1x}
            path2x={track.artwork_path_2x}
            size="full"
            className="w-full h-full scale-150 blur-3xl"
            alt=""
            rounded="md"
          />
        ) : (
          <div className="w-full h-full bg-gradient-to-br from-zinc-800 to-zinc-950" />
        )}
        <div className="absolute inset-0 bg-black/65" />
      </div>

      {/* Foreground */}
      <div className="relative h-full flex flex-col text-white">
        {/* Header */}
        <div className="flex items-center justify-between px-8 py-6 shrink-0">
          <div className="flex items-center gap-4 min-w-0">
            <button
              type="button"
              onClick={onOpenNowPlaying}
              aria-label={t("playerBar.openFullscreen")}
              title={t("playerBar.openFullscreen")}
              className="shrink-0 rounded-lg focus:outline-none focus-visible:ring-2 focus-visible:ring-white/70"
            >
              <Artwork
                path={track.artwork_path}
                path1x={track.artwork_path_1x}
                path2x={track.artwork_path_2x}
                size="1x"
                className="w-14 h-14 shadow-lg"
                iconSize={20}
                alt={track.title}
                rounded="lg"
              />
            </button>
            <div className="min-w-0">
              <div
                id="fullscreen-lyrics-title"
                className="text-xs uppercase tracking-widest text-white/60 mb-1"
              >
                {t("lyrics.title")}
              </div>
              <div className="text-lg font-bold truncate">{track.title}</div>
              <div className="text-sm text-white/70 truncate">
                {track.artist_name ?? "—"}
              </div>
            </div>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label={t("common.close")}
            className="p-2.5 rounded-full bg-white/10 hover:bg-white/20 transition-colors shrink-0"
          >
            <X size={22} />
          </button>
        </div>

        {/* Lyrics body */}
        <div className="flex-1 overflow-y-auto px-8 scroll-smooth">
          <div className="max-w-3xl mx-auto">
            {isFetching && !payload ? (
              <CenteredMessage text={t("lyrics.loading")} />
            ) : error ? (
              <CenteredMessage text={error} />
            ) : !payload || payload.content.trim() === "" ? (
              <CenteredMessage
                icon={<Music2 size={56} />}
                text={t("lyrics.notFound")}
              />
            ) : isSynced ? (
              <ul className="py-[40vh] space-y-6">
                {lrcLines.map((line, index) => {
                  const distance = Math.abs(index - activeIndex);
                  const isActive = index === activeIndex;
                  const isPast = index < activeIndex;
                  // Smooth fade based on distance from the active
                  // line — feels like Apple Music's lyrics view.
                  const opacity = isActive
                    ? 1
                    : Math.max(0.18, 0.7 - distance * 0.08);
                  const hasWords = isActive && (line.words?.length ?? 0) > 0;
                  return (
                    <li
                      key={`${line.timeMs}-${index}`}
                      ref={(el) => {
                        lineRefs.current[index] = el;
                      }}
                      style={{ opacity }}
                    >
                      <button
                        type="button"
                        onClick={() => onSeek(line)}
                        className={`block w-full text-left text-3xl md:text-4xl font-bold leading-snug cursor-pointer transition-all select-none focus:outline-none focus-visible:ring-2 focus-visible:ring-white/70 rounded ${
                          isActive
                            ? "text-white scale-[1.04]"
                            : isPast
                              ? "text-white/40"
                              : "text-white/70 hover:text-white"
                        }`}
                      >
                        {hasWords ? (
                          <span>
                            {line.words!.map((word, wi) => {
                              const wState =
                                wi === activeWordIndex
                                  ? "active"
                                  : wi < activeWordIndex
                                    ? "past"
                                    : "future";
                              return (
                                <span
                                  key={wi}
                                  style={{
                                    opacity:
                                      wState === "active"
                                        ? 1
                                        : wState === "past"
                                          ? 0.8
                                          : 0.45,
                                    transform:
                                      wState === "active"
                                        ? "scale(1.04)"
                                        : "scale(1)",
                                    display: "inline-block",
                                    transition:
                                      "opacity 150ms ease, transform 150ms ease",
                                  }}
                                >
                                  {word.text}
                                </span>
                              );
                            })}
                          </span>
                        ) : (
                          line.text || " "
                        )}
                      </button>
                    </li>
                  );
                })}
              </ul>
            ) : (
              <p className="text-2xl leading-relaxed text-white/90 whitespace-pre-line py-16 text-center">
                {payload.content}
              </p>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

function CenteredMessage({
  icon,
  text,
}: {
  icon?: React.ReactNode;
  text: string;
}) {
  return (
    <div className="h-[80vh] flex flex-col items-center justify-center text-center text-white/70">
      {icon && <div className="mb-4">{icon}</div>}
      <p className="text-lg">{text}</p>
    </div>
  );
}

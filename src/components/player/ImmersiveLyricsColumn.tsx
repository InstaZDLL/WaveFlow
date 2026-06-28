import { Fragment, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { Music2, AlertCircle, Upload, RefreshCcw } from "lucide-react";
import { Artwork } from "../common/Artwork";
import type { Track } from "../../lib/tauri/track";
import type { LyricsLine, LyricsPayload } from "../../lib/tauri/lyrics";
import { useFullscreenLyricsCentering } from "../../hooks/useFullscreenLyricsCentering";

interface ImmersiveLyricsColumnProps {
  track: Track;
  payload: LyricsPayload | null;
  lrcLines: LyricsLine[];
  isSynced: boolean;
  activeIndex: number;
  activeWordIndex: number;
  isFetching: boolean;
  error: string | null;
  /** Radio: timestamp-stripped static text (overrides `payload.content`
   *  in the non-synced branch). `null` falls back to the raw content. */
  staticText: string | null;
  /** Live Web Radio session — hides the import / refetch CTA (no library
   *  row to attach lyrics to). */
  isRadio: boolean;
  onSeek: (line: LyricsLine) => void;
  onImport: () => void;
  onRefetch: () => void;
  /** Header with cover + title (shown in the classic lyrics-only
   *  fullscreen; hidden in the dual-column layout where the now-playing
   *  column already carries the cover + big title). */
  showHeader?: boolean;
  /** When set, the header cover becomes a button (classic mode uses it to
   *  flip back to the now-playing fullscreen, like the old overlay). */
  onCoverClick?: () => void;
}

/**
 * Right column of the immersive view (issue #328): the synced karaoke
 * lyrics scroller. Lifted from the old `FullscreenLyrics` body, but the
 * fetch/parse/active-line state now arrives as props from the shared
 * `useTrackLyrics` hook (owned by `ImmersiveView`) instead of being
 * threaded from the side panel — so the merged view never double-fetches.
 *
 * Auto-scroll keeps its own ref array here (independent from the side
 * `LyricsPanel`, which scrolls in parallel). A clean miss renders a
 * discreet "No lyrics — Import / Fetch" CTA rather than vanishing, so
 * the column width stays stable across track changes.
 */
export function ImmersiveLyricsColumn({
  track,
  payload,
  lrcLines,
  isSynced,
  activeIndex,
  activeWordIndex,
  isFetching,
  error,
  staticText,
  isRadio,
  onSeek,
  onImport,
  onRefetch,
  showHeader = true,
  onCoverClick,
}: ImmersiveLyricsColumnProps) {
  const { t } = useTranslation();
  // Per-profile opt-in (#168). Default OFF — see the hook.
  const { centered: syncCentered } = useFullscreenLyricsCentering();
  const lineRefs = useRef<Array<HTMLLIElement | null>>([]);

  // Keep the active line vertically centered. Own ref array so this
  // scroller and the side panel can scroll independently.
  useEffect(() => {
    if (!isSynced || activeIndex < 0) return;
    const node = lineRefs.current[activeIndex];
    if (node) {
      node.scrollIntoView({ behavior: "smooth", block: "center" });
    }
  }, [activeIndex, isSynced]);

  return (
    <div className="h-full flex flex-col text-white">
      {showHeader && (
        <div className="px-8 pt-8 pb-4 shrink-0 flex items-center gap-4">
          {onCoverClick ? (
            <button
              type="button"
              onClick={onCoverClick}
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
          ) : (
            <Artwork
              path={track.artwork_path}
              path1x={track.artwork_path_1x}
              path2x={track.artwork_path_2x}
              size="1x"
              className="w-14 h-14 shadow-lg shrink-0"
              iconSize={20}
              alt={track.title}
              rounded="lg"
            />
          )}
          <div className="min-w-0">
            <div className="text-xs uppercase tracking-[0.2em] text-white/40">
              {t("lyrics.title")}
            </div>
            <div className="text-base font-bold text-white truncate">
              {track.title}
            </div>
            <div className="text-sm text-white/50 truncate">
              {track.artist_name ?? "—"}
            </div>
          </div>
        </div>
      )}

      <div className="flex-1 overflow-y-auto px-8 scroll-smooth">
        <div className="max-w-3xl mx-auto">
          {isFetching && !payload ? (
            <CenteredMessage text={t("lyrics.loading")} />
          ) : error ? (
            <CenteredMessage
              icon={<AlertCircle size={56} />}
              text={t("lyrics.fetchError")}
            />
          ) : !payload || payload.content.trim() === "" ? (
            <CenteredMessage
              icon={<Music2 size={56} />}
              text={t("lyrics.notFound")}
              // Discreet recovery CTA — hidden for radio (no library row
              // to import-to / refetch-for).
              actions={
                isRadio ? undefined : (
                  <div className="mt-5 flex items-center justify-center gap-3">
                    <button
                      type="button"
                      onClick={onImport}
                      className="flex items-center gap-2 px-4 py-2 rounded-full bg-white/10 hover:bg-white/20 transition-colors text-sm"
                    >
                      <Upload size={15} />
                      {t("lyrics.actions.import")}
                    </button>
                    <button
                      type="button"
                      onClick={onRefetch}
                      disabled={isFetching}
                      className="flex items-center gap-2 px-4 py-2 rounded-full bg-white/10 hover:bg-white/20 transition-colors text-sm disabled:opacity-50"
                    >
                      <RefreshCcw
                        size={15}
                        className={isFetching ? "animate-spin" : ""}
                      />
                      {t("lyrics.actions.refetch")}
                    </button>
                  </div>
                )
              }
            />
          ) : isSynced ? (
            <ul className="py-[40vh] space-y-6">
              {lrcLines.map((line, index) => {
                const distance = Math.abs(index - activeIndex);
                const isActive = index === activeIndex;
                const isPast = index < activeIndex;
                // Smooth fade based on distance from the active line —
                // feels like Apple Music's lyrics view.
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
                      className={`block w-full text-3xl md:text-4xl font-bold leading-snug cursor-pointer transition-all select-none focus:outline-none focus-visible:ring-2 focus-visible:ring-white/70 rounded ${
                        syncCentered ? "text-center" : "text-left"
                      } ${
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
                            // Render a literal space between adjacent word
                            // boxes — `inline-block` strips the JSX
                            // whitespace and many Enhanced LRC sources omit
                            // spaces between word stamps.
                            return (
                              <Fragment key={wi}>
                                <span
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
                                {wi < line.words!.length - 1 && " "}
                              </Fragment>
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
              {staticText ?? payload.content}
            </p>
          )}
        </div>
      </div>
    </div>
  );
}

function CenteredMessage({
  icon,
  text,
  actions,
}: {
  icon?: React.ReactNode;
  text: string;
  actions?: React.ReactNode;
}) {
  return (
    <div className="h-[80vh] flex flex-col items-center justify-center text-center text-white/70">
      {icon && <div className="mb-4">{icon}</div>}
      <p className="text-lg">{text}</p>
      {actions}
    </div>
  );
}

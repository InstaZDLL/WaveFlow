import { BackgroundVocals, InterludeDots } from "../../player/LyricsVoiceParts";
import { useTranslation } from "react-i18next";
import { usePlayer } from "../../../hooks/usePlayer";
import { usePrefersReducedMotion } from "../../../hooks/usePrefersReducedMotion";
import { useTrackLyrics } from "../../../hooks/useTrackLyrics";
import {
  lyricsHighlightColor,
  useLyricsHighlightColor,
} from "../../../hooks/useLyricsHighlightColor";
import { Fragment, useEffect, useRef } from "react";
import { useKaraokeWordFill } from "../../../hooks/useKaraokeWordFill";
import { wordSeparators } from "../../../lib/tauri/lyrics";

/** Fades the first and last lines out instead of slicing them in half. */
/** The interlude dots, at the mini-player's active-line size. */
function MiniInterlude({ progress }: { progress: number }) {
  return (
    <li className="text-center text-[15px] text-white">
      <InterludeDots progress={progress} />
    </li>
  );
}

const LYRICS_MASK =
  "linear-gradient(to bottom, transparent 0%, #000 20%, #000 80%, transparent 100%)";

/**
 * Lyrics inside the mini-player (issue #580), reworked into a mode of the
 * player rather than a sheet over it (#697).
 *
 * **Mounted only while open, and that is the point.** `useTrackLyrics`
 * fetches on every track change, so keeping it mounted behind a closed
 * overlay would fire a second `fetch_lyrics` per track from this webview
 * on top of the main window's. Unmounting means the cost lands only when
 * the user actually asked for lyrics, and the backend cache absorbs the
 * overlap when both surfaces are open at once. The rework kept that: this
 * takes the cover's slot, it does not sit mounted behind it.
 *
 * **Few lines, one obvious.** At a uniform 12 px the active line differed
 * from its neighbours by a font weight, which at that size the eye cannot
 * find. The current line is now noticeably larger and its neighbours
 * smaller and dimmer, so the word-level fill (`useKaraokeWordFill`) is
 * finally legible — it was always wired in, just invisible.
 *
 * **Centred**, like the desktop lyrics window (#582): both are small
 * glanceable surfaces rather than reading columns, and the two now agree.
 * The side panel and the immersive column stay left-aligned.
 *
 * The window defaults to 280x380 and can be dragged down to 240x320, so
 * there is no room for the side panel's source label, provider picker or
 * the import / refetch / clear actions. Those stay in the main window —
 * this is a reading surface, not an editing one. There is no header row
 * either: the toggle in the top bar already shows which mode is open, and
 * closing is one click on the same button.
 */
export function MiniLyricsStage({ artworkUrl }: { artworkUrl: string | null }) {
  const { t } = useTranslation();
  const { currentTrack } = usePlayer();
  const reducedMotion = usePrefersReducedMotion();
  const {
    payload,
    isFetching,
    error,
    lrcLines,
    isSynced,
    radioPlainText,
    activeIndex,
    activeWordIndex,
    activeBackgroundWordIndex,
    overlapIndex,
    overlapWordIndex,
    overlapBackgroundWordIndex,
    activeInterlude,
    seekToLine,
  } = useTrackLyrics();
  // The colour for the line being sung, chosen in the main window's
  // Settings (#751) — the cross-window bridge (#741) brings a change here
  // without reopening the mini-player.
  const { id: highlightId } = useLyricsHighlightColor();
  const highlightColor = lyricsHighlightColor(highlightId, activeIndex);
  const overlapColor = lyricsHighlightColor(highlightId, overlapIndex);

  // Auto-scroll is view-local by the hook's contract: it owns
  // `activeIndex`, each consumer scrolls its own nodes. This one has its
  // own scroller, so it needs its own ref array.
  const lineRefs = useRef<Array<HTMLLIElement | null>>([]);
  useEffect(() => {
    if (!isSynced || activeIndex < 0) return;
    lineRefs.current[activeIndex]?.scrollIntoView({
      // A reader who asked for less motion gets the jump, not the glide.
      behavior: reducedMotion ? "auto" : "smooth",
      block: "center",
    });
  }, [activeIndex, isSynced, reducedMotion]);

  // Progressive word fill on the active word only — the same hook the
  // immersive column uses, so the sweep stays continuous between the
  // 4 Hz `player:position` events instead of stepping every 250 ms.
  const wordFillRef = useKaraokeWordFill(
    isSynced ? lrcLines[activeIndex]?.words?.[activeWordIndex] : undefined,
  );
  // The overlapping line of a duet sweeps on its own clock.
  const overlapFillRef = useKaraokeWordFill(
    isSynced ? lrcLines[overlapIndex]?.words?.[overlapWordIndex] : undefined,
  );

  // Plain text covers three cases that all render the same way: an
  // unsynced payload, a radio session (whose position can't align to a
  // song joined mid-play, so the hook hands back a timestamp-stripped
  // read), and a synced payload we failed to parse into lines.
  const plainText =
    radioPlainText ?? (isSynced ? null : (payload?.content ?? null));

  const message = (text: string) => (
    <div className="flex h-full items-center justify-center px-4 text-center text-[11px] text-white/60">
      {text}
    </div>
  );

  return (
    <div className="relative h-full w-full overflow-hidden rounded-xl bg-black/30">
      {/* The cover this replaced, blurred behind the words, so the surface
          keeps the colour the window's gradient was sampled from instead of
          going to a flat black sheet. Decorative, and dropped in high
          contrast for the same reason the gradient is: this window's
          legibility must not be decided by the user's music. */}
      {artworkUrl && (
        <img
          src={artworkUrl}
          alt=""
          aria-hidden="true"
          className="wf-mini-lyrics-wash pointer-events-none absolute inset-0 h-full w-full scale-125 object-cover opacity-40 blur-xl"
        />
      )}
      <div className="pointer-events-none absolute inset-0 bg-black/35" />

      <div className="relative h-full">
        {currentTrack == null ? (
          message(t("lyrics.noTrack"))
        ) : isFetching && !payload ? (
          message(t("lyrics.loading"))
        ) : error ? (
          message(t("lyrics.fetchError"))
        ) : isSynced && lrcLines.length > 0 ? (
          <ul
            className="h-full overflow-y-auto scrollbar-hide px-3 space-y-2"
            style={{ maskImage: LYRICS_MASK, WebkitMaskImage: LYRICS_MASK }}
          >
            {/* Half-height spacers so the first and last lines can sit in the
              centre like any other. A fixed padding would be a guess about
              a window height the user can drag. */}
            <li aria-hidden="true" className="pointer-events-none h-1/2" />
            {activeInterlude?.afterIndex === -1 && (
              <MiniInterlude progress={activeInterlude.progress} />
            )}
            {lrcLines.map((line, index) => {
              // Through the interlude after it, a line is sung and done.
              // Lines stay centred for a duet too: a 280-pixel window has
              // no room for two sides.
              const beforeInterlude = activeInterlude?.afterIndex === index;
              // The other voice of a duet, still holding its line, stays lit
              // beside the active one.
              const isOverlap = index === overlapIndex;
              const isActive =
                (index === activeIndex && !beforeInterlude) || isOverlap;
              const isPast =
                (activeIndex >= 0 && index < activeIndex && !isOverlap) ||
                beforeInterlude;
              const lineWordIndex = isOverlap
                ? overlapWordIndex
                : activeWordIndex;
              const lineBackgroundWordIndex = isOverlap
                ? overlapBackgroundWordIndex
                : activeBackgroundWordIndex;
              const lineFillRef = isOverlap ? overlapFillRef : wordFillRef;
              const lineColor = isOverlap ? overlapColor : highlightColor;
              const hasWords = isActive && (line.words?.length ?? 0) > 0;
              const separators = hasWords ? wordSeparators(line.words!) : [];
              return (
                <Fragment key={`${line.timeMs}-${index}`}>
                  <li
                    ref={(el) => {
                      lineRefs.current[index] = el;
                    }}
                  >
                    <button
                      type="button"
                      onClick={() => seekToLine(line)}
                      className={`block w-full rounded text-center leading-snug transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-white/70 ${
                        isActive
                          ? "text-[15px] font-semibold text-white"
                          : isPast
                            ? "text-[11px] text-white/30"
                            : "text-[11px] text-white/55 hover:text-white/85"
                      } ${isActive && lineColor ? "wf-lyrics-highlight" : ""}`}
                      style={
                        isActive && lineColor ? { color: lineColor } : undefined
                      }
                    >
                      {hasWords ? (
                        <span>
                          {line.words!.map((word, wi) => {
                            const isActiveWord = wi === lineWordIndex;
                            // The gap between boxes is a text node outside
                            // them (`inline-block` drops its own trailing
                            // space): the one the source has, a space or
                            // nothing — see `wordSeparators`.
                            return (
                              <Fragment key={wi}>
                                <span className="karaoke-word">
                                  {/* Opacity lives on the layers, never on
                                  the box — a parent's opacity applies to
                                  its whole subtree, so dimming the box
                                  would dim the sung overlay with it and
                                  no fill could ever read as brighter. */}
                                  <span
                                    style={{
                                      opacity:
                                        wi < lineWordIndex
                                          ? 0.8
                                          : isActiveWord
                                            ? 0.5
                                            : 0.45,
                                      transition: "opacity 150ms ease",
                                    }}
                                  >
                                    {word.text}
                                  </span>
                                  {isActiveWord && (
                                    // `aria-hidden` because the base layer
                                    // already carries the text — without it
                                    // a screen reader reads the word twice.
                                    <span
                                      ref={lineFillRef}
                                      aria-hidden="true"
                                      className="karaoke-word__fill"
                                    >
                                      {word.text}
                                    </span>
                                  )}
                                </span>
                                {separators[wi]}
                              </Fragment>
                            );
                          })}
                        </span>
                      ) : (
                        line.text || " "
                      )}
                      {/* Lines the file stamped with this one (#821), lit
                          with it: see LyricsPanel. */}
                      {line.companions?.map((companion, ci) => (
                        <span
                          key={ci}
                          dir="auto"
                          className="block text-[10px] font-normal opacity-80"
                        >
                          {companion}
                        </span>
                      ))}
                      {line.background && (
                        <BackgroundVocals
                          reading={line.background}
                          active={isActive}
                          activeWordIndex={lineBackgroundWordIndex}
                          className="text-[10px] font-normal"
                        />
                      )}
                    </button>
                  </li>
                  {beforeInterlude && (
                    <MiniInterlude progress={activeInterlude.progress} />
                  )}
                </Fragment>
              );
            })}
            <li aria-hidden="true" className="pointer-events-none h-1/2" />
          </ul>
        ) : plainText ? (
          <div
            className="h-full overflow-y-auto scrollbar-hide px-3 py-3"
            style={{ maskImage: LYRICS_MASK, WebkitMaskImage: LYRICS_MASK }}
          >
            <p className="text-center text-xs leading-relaxed text-white/80 whitespace-pre-line">
              {plainText}
            </p>
          </div>
        ) : (
          message(t("miniPlayer.lyrics.empty"))
        )}
      </div>
    </div>
  );
}

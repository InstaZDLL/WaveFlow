import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { usePlayerPosition } from "./usePlayerPosition";
import { useEstimatedKaraoke } from "./useEstimatedKaraoke";
import { estimateLineWords } from "../lib/lyricsWordEstimate";
import { attachAssociatedLyrics } from "../lib/lyricsLocalization";
import {
  findActiveLineIndex,
  findActiveWordIndex,
  findOverlappingLineIndex,
  findInterludes,
  groupSharedStamps,
  splitCaretTranslations,
  isUntimedLrc,
  parseLyrics,
  type LyricsInterlude,
  type LyricsLine,
  type LyricsPayload,
  type LyricsWord,
} from "../lib/tauri/lyrics";

const NO_LINES: LyricsLine[] = [];

/** An interlude being played through, with how far into it (0 to 1). */
export interface ActiveInterlude extends LyricsInterlude {
  progress: number;
}

/**
 * A lyrics payload as the surfaces draw it, followed along the playback
 * position: the parsed lines, the line and word being sung, the other
 * voice of a duet, the interlude being played through.
 *
 * Split out of {@link useTrackLyrics} so the lyrics editor's preview
 * draws a document it has not saved yet through the same reading as the
 * panel and the immersive view: a payload is a payload, wherever it came
 * from. `isRadio` keeps a radio's lyrics static, as there the position
 * is time since tuning in, not time into the song.
 */
export function useLyricsTimeline(
  payload: LyricsPayload | null,
  isRadio: boolean,
) {
  const { i18n } = useTranslation();
  const positionMs = usePlayerPosition();

  const rawLines = useMemo<LyricsLine[]>(() => {
    if (!payload) return [];
    return parseLyrics(payload.content, payload.format);
  }, [payload]);

  // Stamped LRC whose lines all sit on one time is untimed: it is handed
  // out as the plain text it really is, which every surface already
  // renders, rather than as synced lines of which only the last lights.
  const untimed =
    payload != null &&
    (payload.format === "lrc" || payload.format === "enhanced_lrc") &&
    isUntimedLrc(rawLines);
  // Lines sharing a stamp light together (#821). After the untimed
  // check, which must see every line the file has: a file stamping all of
  // them `[00:00.00]` is plain text, not one group of a hundred lines.
  //
  // The translation and pronunciation a plugin sent beside the lyrics
  // are laid under the lines they share a stamp with, where the
  // localization toggle finds them as it finds a TTML document's own.
  const uiLanguage = i18n.language;
  const groupedLines = useMemo<LyricsLine[]>(() => {
    if (payload == null) return rawLines;
    const grouped =
      payload.format === "lrc" || payload.format === "enhanced_lrc"
        ? splitCaretTranslations(groupSharedStamps(rawLines))
        : rawLines;
    return attachAssociatedLyrics(grouped, payload.associated, uiLanguage);
  }, [payload, rawLines, uiLanguage]);
  const parsedLines = untimed ? NO_LINES : groupedLines;
  const shownPayload = useMemo<LyricsPayload | null>(
    () =>
      untimed && payload
        ? {
            ...payload,
            content: rawLines.map((line) => line.text).join("\n"),
            format: "plain",
          }
        : payload,
    [untimed, payload, rawLines],
  );

  // Radio is always rendered statically (no karaoke scroll), even when
  // the fetched content is synced LRC — the stream position is "seconds
  // since I tuned in", not "seconds into the song", so a highlight would
  // be wrong.
  const isSynced = !isRadio && parsedLines.length > 0;

  // For radio, strip the LRC timestamps for a clean static read: reuse
  // the parsed lines' text, or fall back to the raw content when it was
  // already plain.
  const radioPlainText = useMemo<string | null>(() => {
    if (!isRadio || !payload) return null;
    if (rawLines.length > 0) return rawLines.map((l) => l.text).join("\n");
    return payload.content;
  }, [isRadio, payload, rawLines]);

  // ── Active-line tracking (auto-scroll lives in each consumer) ─────
  const [activeIndex, setActiveIndex] = useState(-1);
  useEffect(() => {
    if (!isSynced) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setActiveIndex(-1);
      return;
    }
    const idx = findActiveLineIndex(
      parsedLines,
      positionMs,
      Math.max(activeIndex, 0),
    );
    if (idx !== activeIndex) {
      setActiveIndex(idx);
    }
  }, [positionMs, parsedLines, isSynced, activeIndex]);

  // Estimated word timing for line-synced lyrics (#716), opt-in. Only
  // the active line gets words, re-derived each time a line becomes
  // active and never stored, so a lyric the user reads plainly costs
  // nothing and switching the setting off leaves no trace. A line that
  // already carries real word timing is left exactly as it is.
  const estimateWords = useEstimatedKaraoke();
  // The other voice of a duet, while it holds the end of its line. Found
  // on the parsed lines: estimating words never moves a line's bounds.
  const overlapIndex = useMemo(
    () =>
      isSynced && activeIndex >= 0
        ? findOverlappingLineIndex(parsedLines, activeIndex, positionMs)
        : -1,
    [isSynced, parsedLines, activeIndex, positionMs],
  );
  const lrcLines = useMemo<LyricsLine[]>(() => {
    if (!estimateWords || !isSynced || activeIndex < 0) return parsedLines;
    let next: LyricsLine[] | null = null;
    // The overlapping line keeps words too, estimated exactly as when it
    // was active, so handing over does not reshape them mid-sweep.
    for (const index of [activeIndex, overlapIndex]) {
      const line = parsedLines[index];
      if (!line || (line.words?.length ?? 0) > 0) continue;
      const words = estimateLineWords(line, estimateBound(parsedLines, index));
      if (words.length === 0) continue;
      next ??= parsedLines.slice();
      next[index] = { ...line, words };
    }
    return next ?? parsedLines;
  }, [estimateWords, isSynced, activeIndex, overlapIndex, parsedLines]);

  // Active word inside the active line — only computed when the line
  // carries `words[]` so plain LRC stays cheap.
  const activeLine = activeIndex >= 0 ? lrcLines[activeIndex] : undefined;
  const activeWordIndex = useMemo(() => {
    if (!activeLine?.words || activeLine.words.length === 0) return -1;
    return findActiveWordIndex(activeLine.words, positionMs);
  }, [activeLine, positionMs]);

  // Background vocals keep their own clock — they often start before
  // the lead — so their word is found from the position, not borrowed
  // from the lead's index.
  const activeBackgroundWordIndex = useMemo(() => {
    const words = activeLine?.background?.words;
    if (!words || words.length === 0) return -1;
    return findActiveWordIndex(words, positionMs);
  }, [activeLine, positionMs]);

  const overlapLine = overlapIndex >= 0 ? lrcLines[overlapIndex] : undefined;
  const overlapWordIndex = useMemo(
    () => overlapWordAt(overlapLine?.words, positionMs),
    [overlapLine, positionMs],
  );
  const overlapBackgroundWordIndex = useMemo(
    () => overlapWordAt(overlapLine?.background?.words, positionMs),
    [overlapLine, positionMs],
  );

  const interludes = useMemo(
    () => (isSynced ? findInterludes(parsedLines) : []),
    [isSynced, parsedLines],
  );
  const activeInterlude = useMemo<ActiveInterlude | null>(() => {
    const gap = interludes.find(
      (g) => positionMs >= g.startMs && positionMs < g.endMs,
    );
    if (!gap) return null;
    return {
      ...gap,
      progress: (positionMs - gap.startMs) / (gap.endMs - gap.startMs),
    };
  }, [interludes, positionMs]);

  return {
    /** The payload as shown: stamped LRC that is not really timed comes
     *  back as the plain text it is. */
    shownPayload,
    lrcLines,
    isSynced,
    radioPlainText,
    activeIndex,
    activeWordIndex,
    activeLine,
    activeBackgroundWordIndex,
    overlapIndex,
    overlapWordIndex,
    overlapBackgroundWordIndex,
    interludes,
    activeInterlude,
  };
}

/**
 * Where a line's estimated words have to fit: the next line's start, or
 * the line's own stated end when it runs past it — the one voice of a
 * duet holding its end while the other starts. Undefined for the last
 * line, as before.
 */
function estimateBound(lines: LyricsLine[], index: number): number | undefined {
  const next = lines[index + 1]?.timeMs;
  // The lead's own end: the line's may include background vocals that
  // outlast it, and those are not the words being estimated.
  const end = lines[index].leadEndMs ?? lines[index].endMs;
  return next !== undefined && end > next ? end : next;
}

/**
 * The word being sung in the overlapping line, or `words.length` once its
 * last word has ended: such a line stays lit while its background vocals
 * carry on, and its own words are then all sung. The active line keeps
 * `findActiveWordIndex`'s rule — its last word is held until the next
 * line takes over — which is what the estimated fill relies on.
 */
function overlapWordAt(
  words: LyricsWord[] | undefined,
  positionMs: number,
): number {
  if (!words || words.length === 0) return -1;
  // `endMs`, not `fillEndMs`: the fill may finish early, but the word is
  // still the one being sung until its own end.
  const end = words[words.length - 1].endMs;
  if (end >= 0 && positionMs >= end) return words.length;
  return findActiveWordIndex(words, positionMs);
}

/**
 * Mock playback state for the prototype.
 *
 * The real app keeps this in `PlayerContext` and ticks the position
 * from backend events at 4 Hz; here a timer stands in. Nothing talks
 * to Tauri — the prototype has to render in a plain browser tab so it
 * can be reviewed with `bun run dev`.
 */
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { dominantColor } from "../../lib/dominantColor";
import { copyFor, pickLang } from "./copy";
import {
  atmosphereFromPalette,
  atmosphereFromRgb,
  type Atmosphere,
} from "./color";
import { fixtures, type FeTrack, type MoodId } from "./fixtures";
import { FeContext, type FeState } from "./useFe";

const TICK_MS = 250;

export function FeStateProvider({
  children,
  language,
}: {
  children: ReactNode;
  language: string | undefined;
}) {
  const lang = pickLang(language);
  const copy = useMemo(() => copyFor(lang), [lang]);

  const [current, setCurrent] = useState<FeTrack>(fixtures.recentPlays[0]);
  const [queue, setQueue] = useState<FeTrack[]>(
    fixtures.recentPlays.slice(1, 7),
  );
  const [isPlaying, setIsPlaying] = useState(false);
  // The sample resumes a third of the way in, as a real "resume" would.
  const [positionMs, setPositionMs] = useState(() =>
    Math.round(fixtures.recentPlays[0].durationMs * 0.37),
  );
  // Mirrors `positionMs` for the clock, which must not re-arm 4× a second.
  const positionRef = useRef(0);
  const [radioMood, setRadioMood] = useState<MoodId | null>(null);
  const [liked, setLiked] = useState(false);
  const [atmosphere, setAtmosphere] = useState<Atmosphere>(() =>
    atmosphereFromPalette(fixtures.recentPlays[0].palette),
  );

  // Read by the actions below instead of closing over state, so the
  // updaters stay pure — StrictMode runs them twice in development.
  // Written synchronously by `switchTo`, the only place `current` and
  // `queue` change, so two calls in the same tick see each other.
  const currentRef = useRef(current);
  const queueRef = useRef(queue);

  const switchTo = useCallback((track: FeTrack, nextQueue: FeTrack[]) => {
    currentRef.current = track;
    queueRef.current = nextQueue;
    setCurrent(track);
    setQueue(nextQueue);
    setPositionMs(0);
    setAtmosphere(atmosphereFromPalette(track.palette));
  }, []);

  const next = useCallback(() => {
    const [head, ...rest] = queueRef.current;
    if (head) switchTo(head, rest);
  }, [switchTo]);

  // Position clock, only while playing. Reaching the end advances to
  // the next queued track, or stops at the start when the queue is empty.
  useEffect(() => {
    if (!isPlaying) return;
    const id = window.setInterval(() => {
      const ended =
        positionRef.current + TICK_MS >= currentRef.current.durationMs;
      if (!ended) {
        setPositionMs((p) => p + TICK_MS);
        return;
      }
      if (queueRef.current.length > 0) {
        next();
      } else {
        setPositionMs(0);
        setIsPlaying(false);
      }
    }, TICK_MS);
    return () => window.clearInterval(id);
  }, [isPlaying, next]);

  // Live colour sample of the cover, through the app's own extractor,
  // refining the precomputed palette once the image has decoded.
  const sampleSeq = useRef(0);
  useEffect(() => {
    const base = atmosphereFromPalette(current.palette);
    // Bumped before the early return too: a sample still in flight for
    // the previous cover must not land on a track that has none.
    const seq = ++sampleSeq.current;
    if (!current.cover) return;
    dominantColor(current.cover, "vibrant")
      .then((rgb) => {
        if (seq !== sampleSeq.current) return;
        setAtmosphere(atmosphereFromRgb(rgb, base));
      })
      .catch(() => {
        // No colourful pixels, or the cover is missing on this machine:
        // the precomputed palette already stands.
      });
  }, [current.cover, current.palette]);

  const play = useCallback<FeState["play"]>(
    (track, opts) => {
      const prev = currentRef.current;
      if (prev.trackId !== track.trackId) {
        switchTo(
          track,
          [
            prev,
            ...queueRef.current.filter((t) => t.trackId !== track.trackId),
          ].slice(0, 6),
        );
      }
      setRadioMood(opts?.radio ?? null);
      setIsPlaying(true);
    },
    [switchTo],
  );

  const togglePlay = useCallback(() => setIsPlaying((p) => !p), []);
  const seek = useCallback((ms: number) => setPositionMs(Math.max(0, ms)), []);
  useEffect(() => {
    positionRef.current = positionMs;
  }, [positionMs]);

  const previous = useCallback(() => setPositionMs(0), []);
  const toggleLike = useCallback(() => setLiked((l) => !l), []);

  const value = useMemo<FeState>(
    () => ({
      lang,
      copy,
      current,
      queue,
      isPlaying,
      positionMs,
      radioMood,
      atmosphere,
      liked,
      play,
      togglePlay,
      seek,
      next,
      previous,
      toggleLike,
    }),
    [
      lang,
      copy,
      current,
      queue,
      isPlaying,
      positionMs,
      radioMood,
      atmosphere,
      liked,
      play,
      togglePlay,
      seek,
      next,
      previous,
      toggleLike,
    ],
  );

  return <FeContext.Provider value={value}>{children}</FeContext.Provider>;
}

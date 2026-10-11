import {
  Fragment,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Lock, X } from "lucide-react";

import { usePlayer } from "../../hooks/usePlayer";
import { usePlayerPosition } from "../../hooks/usePlayerPosition";
import { useTrackLyrics } from "../../hooks/useTrackLyrics";
import { useKaraokeWordFill } from "../../hooks/useKaraokeWordFill";
import {
  desktopLyricsLinePadding,
  desktopLyricsOutline,
  useDesktopLyricsStyle,
} from "../../hooks/useDesktopLyricsStyle";
import { useDesktopLyricsStatus } from "../../hooks/useDesktopLyricsStatus";
import {
  closeDesktopLyrics,
  setDesktopLyricsBounds,
} from "../../lib/tauri/desktopLyrics";
import type { LyricsLine } from "../../lib/tauri/lyrics";

/** The API declares this type but does not export it. */
type ResizeDirection = Parameters<
  ReturnType<typeof getCurrentWindow>["startResizeDragging"]
>[0];

/** How close to the window's edge, in px, a press resizes rather than
 *  moves it. */
const RESIZE_EDGE = 8;

/**
 * The window edge — or corner — under a point, or `null` inside.
 *
 * The overlay is undecorated and every press on it starts a window drag,
 * so nothing was left to grab the sides with: the window could not be
 * widened, and a long line was cut (#735). A press this close to an edge
 * resizes instead, and the cursor says so first.
 */
function edgeAt(x: number, y: number): ResizeDirection | null {
  const w = window.innerWidth;
  const h = window.innerHeight;
  const north = y < RESIZE_EDGE;
  const south = y >= h - RESIZE_EDGE;
  const west = x < RESIZE_EDGE;
  const east = x >= w - RESIZE_EDGE;
  if (north && west) return "NorthWest";
  if (north && east) return "NorthEast";
  if (south && west) return "SouthWest";
  if (south && east) return "SouthEast";
  if (north) return "North";
  if (south) return "South";
  if (west) return "West";
  if (east) return "East";
  return null;
}

const EDGE_CURSOR: Record<ResizeDirection, string> = {
  North: "ns-resize",
  South: "ns-resize",
  East: "ew-resize",
  West: "ew-resize",
  NorthEast: "nesw-resize",
  SouthWest: "nesw-resize",
  NorthWest: "nwse-resize",
  SouthEast: "nwse-resize",
};

/** Size of the second line relative to the first. */
const SECOND_LINE_SCALE = 0.62;

/** How far a line may shrink to fit the window before it is cut with an
 *  ellipsis instead: below this it stops being readable at a glance. */
const MIN_FIT_SCALE = 0.6;

/**
 * Both lines share this: room for descenders and the outline, one line,
 * cut with an ellipsis only past {@link MIN_FIT_SCALE}.
 *
 * `truncate` clips at the element's box, and `leading-tight` made that box
 * shorter than the glyphs: the bottom of `g`, `y` and `p` was cut off, on
 * Linux in particular where the font's descent is deeper (#673). A roomier
 * line height keeps the descenders inside the box, and the padding, set
 * inline from `desktopLyricsLinePadding`, keeps the outline inside it at
 * any font size and outline width.
 */
const LINE_CLASS =
  "desktop-lyrics-line max-w-full truncate text-center leading-snug";

/**
 * The floating desktop lyrics window (issue #582): the line being sung,
 * and under it either that line's translation or the next line.
 *
 * It follows the song and nothing else. There is no scrolling list, no
 * seeking and no editing — those stay in the main window — because the
 * window is meant to sit over other applications and be read at a
 * glance, and because while it is locked it cannot receive a click at
 * all.
 *
 * Unlocked, hovering shows a frame, a lock button and a close button,
 * and dragging anywhere moves the window. Locked, the backend has turned
 * mouse input off entirely, so nothing here can offer a way back:
 * unlocking happens from the tray, the player bar's "⋯" menu or
 * Settings.
 */
export function DesktopLyrics() {
  const { t } = useTranslation();
  const { currentTrack, playbackState, durationMs } = usePlayer();
  const { lrcLines, isSynced, activeIndex, activeWordIndex } = useTrackLyrics();
  const { style } = useDesktopLyricsStyle();
  const { status, setLocked } = useDesktopLyricsStatus();
  const [hovered, setHovered] = useState(false);
  const [focusedWithin, setFocusedWithin] = useState(false);
  const [edge, setEdge] = useState<ResizeDirection | null>(null);

  usePersistBounds();

  const activeLine: LyricsLine | undefined =
    isSynced && activeIndex >= 0 ? lrcLines[activeIndex] : undefined;
  const wordFillRef = useKaraokeWordFill(activeLine?.words?.[activeWordIndex]);

  // What the two rows say. Unsynced lyrics cannot follow the song, so
  // they are not shown here at all: the title and artist stand in, the
  // same as before the first line and when nothing is playing.
  let second: string | null = null;
  let first: ReactNode;
  if (activeLine) {
    first = renderLine(activeLine, activeWordIndex, wordFillRef);
    // The row under the line: its translation, or — unless that preview
    // is turned off (#735) — the next line. A line the LRC stamped with
    // this one (#821) is its translation as far as the file is concerned;
    // only the first fits the overlay's two rows.
    const translation = activeLine.translation ?? activeLine.companions?.[0];
    second =
      style.showTranslation && translation
        ? translation
        : style.showNextLine
          ? (lrcLines[activeIndex + 1]?.text ?? null)
          : null;
  } else if (currentTrack) {
    first = currentTrack.title;
    second =
      isSynced && style.showNextLine
        ? (lrcLines[0]?.text ?? null)
        : (currentTrack.artist_name ?? null);
  } else {
    first = "WaveFlow";
  }

  const shadow = desktopLyricsOutline(style.outlineWidth);
  const padding = desktopLyricsLinePadding(style.outlineWidth);
  // Focus counts as much as the pointer: the buttons are reachable from
  // the keyboard, and a frame that only appears under the mouse would
  // leave a keyboard user tabbing through controls they cannot see.
  const showChrome = (hovered || focusedWithin) && !status.locked;
  const background = showChrome
    ? Math.max(style.backgroundOpacity, 35)
    : style.backgroundOpacity;
  // Faded out while nothing is being played, when the user asked for it
  // (#814). `loading` is the gap between two tracks, not a pause: the
  // window stays up through it. The frame brings it back under the
  // pointer, so an unlocked window the user cannot see can still be
  // found, moved and closed.
  const faded =
    style.hideOnPause &&
    !showChrome &&
    (playbackState === "paused" ||
      playbackState === "idle" ||
      playbackState === "ended");

  const rootStyle: CSSProperties = {
    backgroundColor: `rgba(0, 0, 0, ${background / 100})`,
    "--dl-text": style.textColor,
    "--dl-highlight": style.highlightColor,
  } as CSSProperties;

  return (
    // Two layers. This outer one is square and fills the window: it takes
    // the pointer, so the resize corners are inside it — a rounded box is
    // hit-tested along its curve, and a press in a corner would miss it.
    // The inner one carries the rounded panel and the text.
    <div
      className={`h-screen w-screen select-none ${
        showChrome ? "cursor-move" : "cursor-default"
      }`}
      style={showChrome && edge ? { cursor: EDGE_CURSOR[edge] } : undefined}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => {
        setHovered(false);
        setEdge(null);
      }}
      onMouseMove={(e) => {
        if (status.locked) return;
        const next = edgeAt(e.clientX, e.clientY);
        if (next !== edge) setEdge(next);
      }}
      onFocus={() => setFocusedWithin(true)}
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) {
          setFocusedWithin(false);
        }
      }}
      onMouseDown={(e) => {
        if (e.button !== 0 || status.locked) return;
        if ((e.target as HTMLElement).closest("button")) return;
        const direction = edgeAt(e.clientX, e.clientY);
        if (direction) {
          getCurrentWindow()
            .startResizeDragging(direction)
            .catch((err) =>
              console.error("[DesktopLyrics] startResizeDragging failed", err),
            );
          return;
        }
        getCurrentWindow()
          .startDragging()
          .catch((err) =>
            console.error("[DesktopLyrics] startDragging failed", err),
          );
      }}
    >
      <div
        className={`group relative flex h-full w-full flex-col items-center justify-center overflow-hidden rounded-2xl px-6 transition-[opacity,background-color] duration-500 motion-reduce:transition-none ${
          showChrome ? "ring-1 ring-inset ring-white/25" : ""
        } ${faded ? "opacity-0" : "opacity-100"}`}
        style={rootStyle}
      >
        {/* Mounted whenever unlocked, not only while shown, so the buttons
          stay in the tab order; locked, the window takes no input at all. */}
        {!status.locked && (
          <div
            className={`absolute right-2 top-2 flex items-center gap-1 transition-opacity ${
              showChrome ? "opacity-100" : "opacity-0"
            }`}
          >
            <button
              type="button"
              onClick={() => setLocked(true)}
              aria-label={t("desktopLyrics.lock")}
              title={t("desktopLyrics.lockHint")}
              className="rounded-full p-1.5 text-white/80 hover:bg-white/15 hover:text-white"
            >
              <Lock size={14} />
            </button>
            <button
              type="button"
              onClick={() => {
                closeDesktopLyrics().catch((err) =>
                  console.error("[DesktopLyrics] close failed", err),
                );
              }}
              aria-label={t("common.close")}
              title={t("common.close")}
              className="rounded-full p-1.5 text-white/80 hover:bg-white/15 hover:text-white"
            >
              <X size={14} />
            </button>
          </div>
        )}

        <FitLine
          size={style.fontSize}
          fitKey={`${padding}:${currentTrack?.id ?? ""}:${activeIndex}:${activeLine?.text ?? currentTrack?.title ?? ""}`}
          className={`${LINE_CLASS} font-bold`}
          style={{
            color: activeLine?.words?.length
              ? "var(--dl-text)"
              : activeLine
                ? "var(--dl-highlight)"
                : "var(--dl-text)",
            textShadow: shadow,
            padding,
          }}
        >
          {first}
        </FitLine>
        {second && (
          <FitLine
            size={Math.round(style.fontSize * SECOND_LINE_SCALE)}
            fitKey={`${padding}:${second}`}
            className={`${LINE_CLASS} font-semibold`}
            style={{
              color: "var(--dl-text)",
              opacity: 0.85,
              textShadow: shadow,
              padding,
            }}
          >
            {second}
          </FitLine>
        )}
        {style.showProgress && durationMs > 0 && (
          <ProgressBar
            durationMs={durationMs}
            thickness={Math.max(3, Math.round(style.fontSize / 12))}
            outlined={style.outlineWidth > 0}
          />
        )}
      </div>
    </div>
  );
}

/**
 * How far into the track playback is, as a thin bar under the lyrics.
 * Its own component so the position, which ticks four times a second,
 * re-renders this bar and not the lines above it. Hidden for a stream
 * with no length (radio), where there is nothing to measure against.
 */
function ProgressBar({
  durationMs,
  thickness,
  outlined,
}: {
  durationMs: number;
  thickness: number;
  outlined: boolean;
}) {
  const positionMs = usePlayerPosition();
  const fraction = Math.min(1, Math.max(0, positionMs / durationMs));
  return (
    <div
      aria-hidden="true"
      className="mt-[0.4em] w-full shrink-0 overflow-hidden rounded-full"
      style={{
        height: thickness,
        // The track in the text colour, faint; a dark rim when the text
        // has an outline, so the bar stays visible on a light desktop too.
        backgroundColor:
          "color-mix(in srgb, var(--dl-text) 25%, transparent)",
        boxShadow: outlined ? "0 0 0 1px rgba(0,0,0,0.55)" : undefined,
      }}
    >
      <div
        className="h-full w-full origin-left rounded-full transition-transform duration-250 ease-linear motion-reduce:transition-none"
        style={{
          backgroundColor: "var(--dl-highlight)",
          transform: `scaleX(${fraction})`,
        }}
      />
    </div>
  );
}

/**
 * One line of the overlay, shrunk to fit the window when it is too long
 * at the chosen size (#673), down to {@link MIN_FIT_SCALE}. Past that it
 * keeps the floor and `truncate` cuts it.
 *
 * The natural width is measured at the chosen size, not the size last
 * applied: measuring the already-shrunk line would find it fits and grow
 * it back, and the two would alternate. The inline size is restored right
 * after, so React's own value is the one left in place. `fitKey` changes
 * with the text, the observer covers the window being resized, and a font
 * finishing its load refits too.
 */
function FitLine({
  size,
  fitKey,
  className,
  style,
  children,
}: {
  size: number;
  fitKey: string;
  className: string;
  style: CSSProperties;
  children: ReactNode;
}) {
  const ref = useRef<HTMLParagraphElement>(null);
  const [scale, setScale] = useState(1);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const fit = () => {
      const applied = el.style.fontSize;
      el.style.fontSize = `${size}px`;
      const natural = el.scrollWidth;
      const room = el.clientWidth;
      // Both widths include the padding, which does not shrink with the
      // font when the outline width sets it: scaling it along with the
      // text would leave the line a few px too wide, and cut. Only the
      // text scales, so only the text is compared. An `em` padding does
      // shrink, which errs toward a line slightly smaller than needed.
      const computed = getComputedStyle(el);
      const pad =
        parseFloat(computed.paddingLeft) + parseFloat(computed.paddingRight);
      el.style.fontSize = applied;
      const text = natural - pad;
      const textRoom = room - pad;
      const next =
        natural > room && text > 0
          ? Math.max(MIN_FIT_SCALE, Math.max(0, textRoom) / text)
          : 1;
      // Always the measured value: the measurement is taken at the chosen
      // size, so it cannot feed back on itself, and rounding a slight
      // overflow away would leave that line cut by a letter.
      setScale(next);
    };
    fit();
    const observer = new ResizeObserver(fit);
    observer.observe(el.parentElement ?? el);
    // The fonts come from `@fontsource` and load after the first paint.
    // A first fit taken in the fallback face is wrong once the real one
    // arrives, and a font swap resizes nothing the observer watches.
    let active = true;
    const refit = () => {
      if (active) fit();
    };
    void document.fonts.ready.then(refit);
    document.fonts.addEventListener("loadingdone", refit);
    return () => {
      active = false;
      observer.disconnect();
      document.fonts.removeEventListener("loadingdone", refit);
    };
  }, [size, fitKey]);

  return (
    <p
      ref={ref}
      className={className}
      style={{ ...style, fontSize: size * scale }}
    >
      {children}
    </p>
  );
}

/**
 * The current line, word by word when it is word-timed: words already
 * sung take the highlight colour, the one being sung fills across, the
 * rest keep the text colour. Same two-layer technique as the immersive
 * column (`.karaoke-word` in `app.css`).
 */
function renderLine(
  line: LyricsLine,
  activeWordIndex: number,
  wordFillRef: (el: HTMLElement | null) => void,
) {
  if (!line.words || line.words.length === 0) return line.text || " ";
  return line.words.map((word, wi) => {
    const sung = wi < activeWordIndex;
    const active = wi === activeWordIndex;
    return (
      <Fragment key={wi}>
        <span className="karaoke-word">
          <span style={{ color: sung ? "var(--dl-highlight)" : undefined }}>
            {word.text}
          </span>
          {active && (
            <span
              ref={wordFillRef}
              aria-hidden="true"
              className="karaoke-word__fill"
              style={{ color: "var(--dl-highlight)" }}
            >
              {word.text}
            </span>
          )}
        </span>
        {wi < line.words!.length - 1 && " "}
      </Fragment>
    );
  });
}

/**
 * Save position and size a moment after the user stops moving or
 * resizing, like the mini-player: `onMoved` fires continuously during a
 * drag, and the backend reads the last saved rectangle when it next
 * creates the window.
 */
function usePersistBounds() {
  useEffect(() => {
    const win = getCurrentWindow();
    let timer: number | null = null;
    const offs: Array<() => void> = [];
    let cancelled = false;

    const schedule = () => {
      if (timer != null) window.clearTimeout(timer);
      timer = window.setTimeout(async () => {
        try {
          const scale = await win.scaleFactor();
          const pos = await win.outerPosition();
          const size = await win.outerSize();
          await setDesktopLyricsBounds({
            x: pos.x / scale,
            y: pos.y / scale,
            width: size.width / scale,
            height: size.height / scale,
          });
        } catch (err) {
          console.error("[DesktopLyrics] persist bounds failed", err);
        }
      }, 300);
    };

    for (const subscribe of [win.onMoved.bind(win), win.onResized.bind(win)]) {
      subscribe(schedule)
        .then((off) => {
          if (cancelled) off();
          else offs.push(off);
        })
        .catch((err) =>
          console.error("[DesktopLyrics] bounds listener failed", err),
        );
    }

    return () => {
      cancelled = true;
      if (timer != null) window.clearTimeout(timer);
      for (const off of offs) off();
    };
  }, []);
}

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
import { useTrackLyrics } from "../../hooks/useTrackLyrics";
import { useKaraokeWordFill } from "../../hooks/useKaraokeWordFill";
import { useDesktopLyricsStyle } from "../../hooks/useDesktopLyricsStyle";
import { useDesktopLyricsStatus } from "../../hooks/useDesktopLyricsStatus";
import {
  closeDesktopLyrics,
  setDesktopLyricsBounds,
} from "../../lib/tauri/desktopLyrics";
import type { LyricsLine } from "../../lib/tauri/lyrics";

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
 * line height and a padding sized in `em` keep the descenders and the
 * text-shadow outline inside the box at any font size.
 */
const LINE_CLASS =
  "desktop-lyrics-line max-w-full truncate text-center leading-snug px-[0.15em] py-[0.1em]";

/**
 * An outline drawn with eight text shadows rather than
 * `-webkit-text-stroke`. A stroke is centred on the glyph edge, so it
 * eats into thin letters at small sizes and needs `paint-order` to sit
 * behind the fill, which HTML text does not honour everywhere. Shadows
 * only ever add outside the glyph, on every webview WaveFlow ships in.
 */
function outlineShadow(px: number): string {
  const d = Math.max(1, Math.round(px));
  const c = "rgba(0,0,0,0.85)";
  return [
    [-d, -d],
    [0, -d],
    [d, -d],
    [-d, 0],
    [d, 0],
    [-d, d],
    [0, d],
    [d, d],
  ]
    .map(([x, y]) => `${x}px ${y}px 0 ${c}`)
    .join(", ");
}

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
  const { currentTrack } = usePlayer();
  const { lrcLines, isSynced, activeIndex, activeWordIndex } = useTrackLyrics();
  const { style } = useDesktopLyricsStyle();
  const { status, setLocked } = useDesktopLyricsStatus();
  const [hovered, setHovered] = useState(false);
  const [focusedWithin, setFocusedWithin] = useState(false);

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
    second =
      style.showTranslation && activeLine.translation
        ? activeLine.translation
        : (lrcLines[activeIndex + 1]?.text ?? null);
  } else if (currentTrack) {
    first = currentTrack.title;
    second = isSynced
      ? (lrcLines[0]?.text ?? null)
      : (currentTrack.artist_name ?? null);
  } else {
    first = "WaveFlow";
  }

  const shadow = style.outline ? outlineShadow(style.fontSize / 18) : undefined;
  // Focus counts as much as the pointer: the buttons are reachable from
  // the keyboard, and a frame that only appears under the mouse would
  // leave a keyboard user tabbing through controls they cannot see.
  const showChrome = (hovered || focusedWithin) && !status.locked;
  const background = showChrome
    ? Math.max(style.backgroundOpacity, 35)
    : style.backgroundOpacity;

  const rootStyle: CSSProperties = {
    backgroundColor: `rgba(0, 0, 0, ${background / 100})`,
    "--dl-text": style.textColor,
    "--dl-highlight": style.highlightColor,
  } as CSSProperties;

  return (
    <div
      className={`group relative flex h-screen w-screen select-none flex-col items-center justify-center overflow-hidden rounded-2xl px-6 transition-colors ${
        showChrome
          ? "cursor-move ring-1 ring-inset ring-white/25"
          : "cursor-default"
      }`}
      style={rootStyle}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      onFocus={() => setFocusedWithin(true)}
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) {
          setFocusedWithin(false);
        }
      }}
      onMouseDown={(e) => {
        if (e.button !== 0 || status.locked) return;
        if ((e.target as HTMLElement).closest("button")) return;
        getCurrentWindow()
          .startDragging()
          .catch((err) =>
            console.error("[DesktopLyrics] startDragging failed", err),
          );
      }}
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
        fitKey={`${currentTrack?.id ?? ""}:${activeIndex}:${isSynced}`}
        className={`${LINE_CLASS} font-bold`}
        style={{
          color: activeLine?.words?.length
            ? "var(--dl-text)"
            : activeLine
              ? "var(--dl-highlight)"
              : "var(--dl-text)",
          textShadow: shadow,
        }}
      >
        {first}
      </FitLine>
      {second && (
        <FitLine
          size={Math.round(style.fontSize * SECOND_LINE_SCALE)}
          fitKey={second}
          className={`${LINE_CLASS} font-semibold`}
          style={{
            color: "var(--dl-text)",
            opacity: 0.85,
            textShadow: shadow,
          }}
        >
          {second}
        </FitLine>
      )}
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
 * with the text, and the observer covers the window being resized.
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
      el.style.fontSize = applied;
      const next =
        natural > room && natural > 0
          ? Math.max(MIN_FIT_SCALE, room / natural)
          : 1;
      setScale((prev) => (Math.abs(prev - next) < 0.01 ? prev : next));
    };
    fit();
    const observer = new ResizeObserver(fit);
    observer.observe(el.parentElement ?? el);
    return () => observer.disconnect();
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

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { useModalA11y } from "../../hooks/useModalA11y";
import { useProfile } from "../../hooks/useProfile";
import { markPlayerTourSeen } from "../../lib/discovery";
import "./discovery.css";

/**
 * The stops, in the order the bar shows them. Each is a `data-tour` mark
 * on a real control of the player bar; a control the current skin or
 * layout does not show (a button unpinned in Settings, the like button
 * with nothing playing) has no mark on screen and is skipped.
 */
const STOPS = [
  "cover",
  "like",
  "abLoop",
  "sleepTimer",
  "eqPreset",
  "lyrics",
  "queue",
  "device",
  "more",
  "volume",
  "miniPlayer",
  "immersive",
] as const;

type Stop = (typeof STOPS)[number];

/** Room left around a control inside the spotlight, in px. */
const SPOT_PAD = 6;
/** Distance between the spotlight and the bubble, in px. */
const BUBBLE_GAP = 14;
const BUBBLE_WIDTH = 300;
/** Distance kept from the window's edges, in px. */
const EDGE = 16;

/** The player bar's control for a stop, if it is on screen. */
function controlOf(stop: Stop): HTMLElement | null {
  const el = document.querySelector<HTMLElement>(
    `footer [data-tour="${stop}"]`,
  );
  if (!el) return null;
  const box = el.getBoundingClientRect();
  return box.width > 0 && box.height > 0 ? el : null;
}

/**
 * A short tour of the player bar: the window dims, a spotlight cut around
 * one control at a time, a bubble says what it does, with Next / Skip and
 * "x / n". The targets are read from the controls themselves, never from
 * coordinates, and followed when the window resizes.
 *
 * Portalled over everything; `useModalA11y` gives it Escape and keeps the
 * focus in it, which lands on "Next" at every stop. The window is dimmed
 * by a flat veil with the spotlight cut out of it; the hole glides between
 * stops, except under reduced motion or the reduced effects level, where
 * it jumps. The tour touches nothing: a
 * control is shown, not pressed, so it ticks none of the first steps.
 */
export function PlayerBarTour({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const profileId = useProfile().activeProfile?.id ?? null;
  // Read once, after the bar has rendered: the stops this layout shows.
  const [stops, setStops] = useState<Stop[] | null>(null);
  const [index, setIndex] = useState(0);
  const [box, setBox] = useState<DOMRect | null>(null);
  const dialogRef = useModalA11y<HTMLDivElement>(true, onClose);

  useLayoutEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setStops(STOPS.filter((stop) => controlOf(stop) != null));
  }, []);

  // Seen once shown, as the tips are counted: a tour left half-way does
  // not come back by itself.
  useEffect(() => {
    if (profileId == null) return;
    markPlayerTourSeen(profileId).catch((err) =>
      console.error("[PlayerBarTour] mark seen failed", err),
    );
  }, [profileId]);

  const stop = stops?.[index] ?? null;
  const measure = useCallback(() => {
    if (!stop || !stops) return;
    const el = controlOf(stop);
    if (!el) {
      // The control went away during the tour (a resize, a bar that
      // changed): on to the next stop, or the end — never an empty overlay
      // left catching the clicks.
      if (index < stops.length - 1) setIndex(index + 1);
      else onClose();
      return;
    }
    setBox(el.getBoundingClientRect());
  }, [stop, stops, index, onClose]);

  useLayoutEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [measure]);

  // Nothing to show (a bar with none of the marks): leave at once.
  useEffect(() => {
    if (stops && stops.length === 0) onClose();
  }, [stops, onClose]);

  // Keyboard users land on "Next" at every stop.
  const nextRef = useRef<HTMLButtonElement>(null);
  const measured = box != null;
  useEffect(() => {
    if (!measured) return;
    // After `useModalA11y`'s own first focus, which is deferred the same
    // way and would otherwise land on the empty overlay.
    const timer = window.setTimeout(() => nextRef.current?.focus(), 0);
    return () => window.clearTimeout(timer);
  }, [stop, measured]);

  const ready = stops != null && stop != null && box != null;
  const last = stops != null && index === stops.length - 1;
  const spot = box
    ? {
        left: box.left - SPOT_PAD,
        top: box.top - SPOT_PAD,
        width: box.width + SPOT_PAD * 2,
        height: box.height + SPOT_PAD * 2,
      }
    : null;
  // The bubble over the control when it sits low in the window (the bar
  // is at the bottom), under it otherwise; kept inside the window.
  const above = box != null && box.top > window.innerHeight / 2;
  const left = box
    ? Math.min(
        Math.max(EDGE, box.left + box.width / 2 - BUBBLE_WIDTH / 2),
        window.innerWidth - BUBBLE_WIDTH - EDGE,
      )
    : EDGE;
  const bubbleStyle =
    spot == null
      ? undefined
      : above
        ? { left, bottom: window.innerHeight - spot.top + BUBBLE_GAP }
        : { left, top: spot.top + spot.height + BUBBLE_GAP };
  // The veil is the window minus the spotlight: one polygon, its hole cut
  // by the even-odd rule, so the dimmed part is flat paint and the hole
  // can glide from stop to stop.
  const veil =
    spot == null
      ? undefined
      : `polygon(evenodd, 0 0, 100% 0, 100% 100%, 0 100%, 0 0, ${spot.left}px ${spot.top}px, ${spot.left + spot.width}px ${spot.top}px, ${spot.left + spot.width}px ${spot.top + spot.height}px, ${spot.left}px ${spot.top + spot.height}px, ${spot.left}px ${spot.top}px)`;

  // The overlay is one element from the first render on — `useModalA11y`
  // traps the focus in it and gives it Escape — empty until the stops are
  // measured. It catches every click: the tour shows the controls, it
  // does not let them be pressed. The bubble is the dialog.
  return createPortal(
    <div
      ref={dialogRef}
      tabIndex={-1}
      className="player-tour fixed inset-0 z-[120] outline-none"
    >
      {ready && spot && (
        <>
          <div
            aria-hidden="true"
            className="player-tour__veil absolute inset-0"
            style={{ clipPath: veil }}
          />
          <div
            aria-hidden="true"
            className="player-tour__spot absolute rounded-xl"
            style={{
              transform: `translate(${spot.left}px, ${spot.top}px)`,
              width: spot.width,
              height: spot.height,
            }}
          />
          <div
            role="dialog"
            aria-modal="true"
            aria-labelledby="player-tour-title"
            aria-describedby="player-tour-body"
            className="player-tour__bubble absolute rounded-2xl border border-zinc-200 bg-white p-4 shadow-2xl dark:border-zinc-800 dark:bg-zinc-900"
            style={{ ...bubbleStyle, width: BUBBLE_WIDTH }}
          >
            {/* Keyed on the stop so each one rises in, without remounting
                the dialog around it. */}
            <div key={stop} className="player-tour__content">
              <div className="flex items-center justify-between gap-3">
                <span className="onboarding-kicker text-[11px] font-semibold uppercase tracking-widest text-zinc-400">
                  {t("playerTour.label")}
                </span>
                <span className="text-[11px] tabular-nums text-zinc-400">
                  {t("playerTour.counter", {
                    current: index + 1,
                    total: stops.length,
                  })}
                </span>
              </div>
              <h2
                id="player-tour-title"
                className="mt-2 text-base font-semibold text-zinc-900 dark:text-white"
              >
                {t(`playerTour.items.${stop}.title`)}
              </h2>
              <p
                id="player-tour-body"
                className="mt-1 text-sm leading-relaxed text-zinc-600 dark:text-zinc-300"
              >
                {t(`playerTour.items.${stop}.body`)}
              </p>
            </div>
            <div className="mt-4 flex items-center justify-end gap-2">
              {!last && (
                <button
                  type="button"
                  onClick={onClose}
                  className="onboarding-ghost rounded-full px-3 py-1.5 text-xs text-zinc-500 transition-colors hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-white"
                >
                  {t("playerTour.skip")}
                </button>
              )}
              <button
                ref={nextRef}
                type="button"
                onClick={() => (last ? onClose() : setIndex((i) => i + 1))}
                className="onboarding-primary rounded-full bg-emerald-500 px-4 py-1.5 text-xs font-semibold text-white transition-colors hover:bg-emerald-600"
              >
                {last ? t("playerTour.finish") : t("playerTour.next")}
              </button>
            </div>
          </div>
        </>
      )}
    </div>,
    document.body,
  );
}

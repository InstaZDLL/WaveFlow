import { useEffect, useRef } from "react";
import { usePrefersReducedMotion } from "../../hooks/usePrefersReducedMotion";

interface WaveFlowLogoProps {
  className?: string;
  style?: React.CSSProperties;
  /** Swell while true (the rail passes "music is playing"); settle when false. */
  animated?: boolean;
}

// The small-size drawing's centre line in a 256 box: one and a half
// periods of a sine between y = MID ± AMP, its ends on the inflection
// points so it reads W and not M. Stroked at 46 with round caps it is
// the kit's outlined symbol, which is why the mark is drawn as a
// stroke: the swell only has to move three heights.
const MID = 124;
const AMP = 60;
// Where the Bézier handles sit, as a share of the amplitude.
const HANDLE = 31.1 / 60;

/** The W with its two troughs and its crest at their own amplitudes. */
function wavePath(trough1: number, crest: number, trough2: number): string {
  const at = (a: number, sign: number) => MID + sign * a;
  const handle = (a: number, sign: number) => MID + sign * a * HANDLE;
  return (
    `M35 ${MID}` +
    ` C45.23 ${handle(trough1, 1)} 54.84 ${at(trough1, 1)} 66 ${at(trough1, 1)}` +
    ` C77.16 ${at(trough1, 1)} 86.77 ${handle(trough1, 1)} 97 ${MID}` +
    ` C107.23 ${handle(crest, -1)} 116.84 ${at(crest, -1)} 128 ${at(crest, -1)}` +
    ` C139.16 ${at(crest, -1)} 148.77 ${handle(crest, -1)} 159 ${MID}` +
    ` C169.23 ${handle(trough2, 1)} 178.84 ${at(trough2, 1)} 190 ${at(trough2, 1)}` +
    ` C201.16 ${at(trough2, 1)} 210.77 ${handle(trough2, 1)} 221 ${MID}`
  );
}

const REST = wavePath(AMP, AMP, AMP);

// The swell: each of the three extrema breathes down to 73 % of its
// height and back, one after the other, as the old logo's bars did.
const PERIOD_MS = 1500;
const DEPTH = 16;
const LAG = 1.05; // radians between one extremum and the next
// How fast a height follows its target: it is what lets a pause settle
// the W instead of freezing it mid-breath.
const SETTLE_MS = 120;
// 30 frames a second is plenty for a 28 px breath, and halves the work.
const FRAME_MS = 33;
// A skin that wants the mark still (Editorial's engraving) says so with
// `--wf-mark-motion: none` on the svg; read about once a second.
const SKIN_CHECK_MS = 1000;

/**
 * The WaveFlow symbol: the W of the name drawn as one and a half periods
 * of a sine, a single monoline stroke, the small-size drawing (stroke
 * 46 / 256) that keeps its troughs open at 16-24 px. It paints in
 * `currentColor` and nothing else, no gradient: the caller or the skin
 * picks the colour (the fluid rail puts it in the accent). With
 * `animated`, its troughs and crest swell in turn; it stays still under
 * `prefers-reduced-motion` and in a skin that asks for it.
 * Decorative: every caller sets the name next to it.
 */
export function WaveFlowLogo({
  className = "w-8 h-8",
  style,
  animated = false,
}: WaveFlowLogoProps) {
  const pathRef = useRef<SVGPathElement>(null);
  // The current heights outlive an effect run, so a pause settles from
  // wherever the breath was.
  const heights = useRef<[number, number, number]>([AMP, AMP, AMP]);
  const reducedMotion = usePrefersReducedMotion();

  useEffect(() => {
    const path = pathRef.current;
    if (!path) return;
    if (reducedMotion) {
      heights.current = [AMP, AMP, AMP];
      path.setAttribute("d", REST);
      return;
    }
    let raf = 0;
    let timer = 0;
    let start = 0;
    let last = 0;
    let lastSkinCheck = -Infinity;
    let skinAllows = true;
    let atRest = false;
    const tick = (now: number) => {
      if (last && now - last < FRAME_MS) {
        raf = requestAnimationFrame(tick);
        return;
      }
      // A hidden window gets no frames: cap the step so it does not jump.
      const dt = last ? Math.min(now - last, 100) : FRAME_MS;
      last = now;
      if (!start) start = now;
      if (now - lastSkinCheck >= SKIN_CHECK_MS) {
        lastSkinCheck = now;
        skinAllows =
          getComputedStyle(path).getPropertyValue("--wf-mark-motion").trim() !==
          "none";
      }
      const swelling = animated && skinAllows;
      const phase = ((now - start) / PERIOD_MS) * Math.PI * 2;
      const follow = Math.min(1, dt / SETTLE_MS);
      const next = heights.current.map((h, i) => {
        const target = swelling
          ? AMP - DEPTH * (0.5 + 0.5 * Math.sin(phase - i * LAG))
          : AMP;
        return h + (target - h) * follow;
      }) as [number, number, number];
      const settled = !swelling && next.every((h) => Math.abs(h - AMP) < 0.05);
      if (settled) {
        heights.current = [AMP, AMP, AMP];
        if (!atRest) path.setAttribute("d", REST);
      } else {
        heights.current = next;
        path.setAttribute("d", wavePath(...next));
      }
      atRest = settled;
      if (!settled) {
        raf = requestAnimationFrame(tick);
      } else if (animated) {
        // Playing in a skin that holds the mark still: no frames, just a
        // look once a second in case the skin changes.
        timer = window.setTimeout(() => {
          raf = requestAnimationFrame(tick);
        }, SKIN_CHECK_MS);
      }
      // Still and not playing: nothing left to ask for.
    };
    raf = requestAnimationFrame(tick);
    return () => {
      cancelAnimationFrame(raf);
      window.clearTimeout(timer);
    };
  }, [animated, reducedMotion]);

  return (
    <svg
      viewBox="0 0 256 256"
      xmlns="http://www.w3.org/2000/svg"
      className={className}
      style={style}
      aria-hidden="true"
      focusable="false"
    >
      <path
        ref={pathRef}
        d={REST}
        fill="none"
        stroke="currentColor"
        strokeWidth={46}
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

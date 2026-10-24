interface WaveFlowLogoProps {
  className?: string;
  style?: React.CSSProperties;
}

/**
 * The WaveFlow symbol: the W of the name drawn as one and a half periods
 * of a sine, a single monoline stroke. This is the small-size drawing
 * (stroke 46 / 256, shallower amplitude) that keeps its troughs open at
 * 16-24 px, outlined so it fills rather than strokes. It paints in
 * `currentColor` and nothing else: no gradient, no pulse — the caller or
 * the skin picks the colour (the fluid rail puts it in the accent).
 * Decorative: every caller sets the name next to it.
 */
export function WaveFlowLogo({
  className = "w-8 h-8",
  style,
}: WaveFlowLogoProps) {
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
        fill="currentColor"
        d="M 128,41 C 115.28,41 106.95,48.53 102.13,54.38 97.31,60.24 94.22,66.24 91.23,72.78 85.26,85.86 80.27,101.24 75.15,116.81 71.96,126.52 68.92,135.29 66,143.17 63.08,135.29 60.04,126.52 56.85,116.81 A 23 23 0 0 0 27.81,102.15 23 23 0 0 0 13.15,131.19 c 5.12,15.57 10.1,30.95 16.08,44.03 2.99,6.54 6.08,12.54 10.9,18.4 C 44.95,199.47 53.28,207 66,207 c 12.72,0 21.05,-7.53 25.87,-13.38 4.82,-5.86 7.91,-11.86 10.9,-18.4 5.97,-13.08 10.96,-28.46 16.08,-44.03 3.19,-9.71 6.24,-18.47 9.15,-26.35 2.92,7.88 5.96,16.65 9.15,26.35 5.12,15.57 10.1,30.95 16.08,44.03 2.99,6.54 6.08,12.54 10.9,18.4 C 168.95,199.47 177.28,207 190,207 c 12.72,0 21.05,-7.53 25.87,-13.38 4.82,-5.86 7.91,-11.86 10.9,-18.4 5.97,-13.08 10.96,-28.46 16.08,-44.03 A 23 23 0 0 0 228.19,102.15 23 23 0 0 0 199.15,116.81 c -3.19,9.71 -6.24,18.47 -9.15,26.35 -2.92,-7.88 -5.96,-16.65 -9.15,-26.35 C 175.73,101.24 170.74,85.86 164.77,72.78 161.78,66.24 158.69,60.24 153.87,54.38 149.05,48.53 140.72,41 128,41 Z"
      />
    </svg>
  );
}

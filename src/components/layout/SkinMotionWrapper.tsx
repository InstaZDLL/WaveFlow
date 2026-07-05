import { type ReactNode, useMemo } from "react";
import { MotionConfig } from "framer-motion";
import { useSkin } from "../../hooks/useSkin";

/**
 * Wrap children in a Framer Motion `<MotionConfig>` whose
 * `transition` defaults reflect the active skin's motion
 * tokens. Every `<motion.div>` subtree that doesn't specify
 * its own `transition` prop picks these up automatically.
 *
 * The motion shape we propagate:
 * - `type: "spring"` — the WaveFlow baseline. Skin chooses
 *   the stiffness + damping so Studio feels snappy, Lounge
 *   glides, Pulse springs.
 * - `duration` field carried alongside for non-spring
 *   callers (page fades, opacity transitions). Framer
 *   honours it for `tween` transitions and ignores it on
 *   springs, so passing both is safe.
 *
 * The wrapper memoises on motion-token identity so a theme
 * swap that doesn't touch the skin doesn't churn the
 * MotionConfig context value.
 */
export function SkinMotionWrapper({ children }: { children: ReactNode }) {
  const { skin } = useSkin();
  const transition = useMemo(
    () => ({
      type: "spring" as const,
      stiffness: skin.motion.springStiffness,
      damping: skin.motion.springDamping,
      duration: skin.motion.duration,
    }),
    [
      skin.motion.springStiffness,
      skin.motion.springDamping,
      skin.motion.duration,
    ],
  );
  return <MotionConfig transition={transition}>{children}</MotionConfig>;
}

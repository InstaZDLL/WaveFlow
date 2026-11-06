import { useEffect, useState } from "react";
import { getTrackAnalysis } from "../lib/tauri/analysis";

/**
 * Whether the analysis caught a library track as a probable fake
 * lossless — a lossless file whose highs stop at a lossy encoder's wall
 * (`track_analysis.cutoff_hz`, see `analysis/bandwidth.rs`).
 *
 * Read once per track from the cached analysis row; nothing is measured
 * here. `undefined` until the answer is in and for anything that has no
 * library row (a radio's negative sentinel id), `false` for a track the
 * analysis cleared or never looked at. The answer is stamped with the
 * track it was read for, so a slow read never lands on the next track.
 */
export function useProbableFakeLossless(
  trackId: number | null | undefined,
): boolean | undefined {
  const [answer, setAnswer] = useState<{ id: number; fake: boolean } | null>(
    null,
  );

  useEffect(() => {
    if (trackId == null || trackId <= 0) return;
    let cancelled = false;
    getTrackAnalysis(trackId)
      .then((analysis) => {
        if (cancelled) return;
        setAnswer({
          id: trackId,
          fake: analysis?.probable_fake_lossless ?? false,
        });
      })
      .catch((err) => {
        console.error("[useProbableFakeLossless] read failed", err);
      });
    return () => {
      cancelled = true;
    };
  }, [trackId]);

  return answer != null && answer.id === trackId ? answer.fake : undefined;
}

import { useEffect } from "react";
import { createPortal } from "react-dom";
import { Users } from "lucide-react";

import { clearWaveSyncNotice, useWaveSync } from "../../hooks/useWaveSync";

const AUTO_HIDE_MS = 5_000;

/**
 * One line about the last WaveSync action that had something to say: tracks
 * added, some that could not be shared, a refusal. A live region that stays
 * mounted, as `WriteFailedToast` is, so a screen reader announces each.
 */
export function WaveSyncNotice() {
  const { notice } = useWaveSync();

  useEffect(() => {
    if (notice == null) return;
    const timer = window.setTimeout(clearWaveSyncNotice, AUTO_HIDE_MS);
    return () => window.clearTimeout(timer);
  }, [notice]);

  return createPortal(
    <div
      role="status"
      aria-live="polite"
      className="fixed bottom-40 left-1/2 -translate-x-1/2 z-100"
    >
      {notice != null && (
        <div
          key={notice.key}
          className="flex items-center gap-2 rounded-full border border-zinc-200 bg-white/95 px-4 py-2 text-sm text-zinc-700 shadow-xl backdrop-blur animate-fade-in dark:border-zinc-700 dark:bg-zinc-900/95 dark:text-zinc-200"
        >
          <Users
            size={16}
            className="shrink-0 text-emerald-500"
            aria-hidden="true"
          />
          <span>{notice.text}</span>
        </div>
      )}
    </div>,
    document.body,
  );
}

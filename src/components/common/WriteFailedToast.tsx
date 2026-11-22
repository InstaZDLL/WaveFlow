import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { AlertTriangle } from "lucide-react";

import {
  WRITE_FAILED_EVENT,
  type WriteFailedDetail,
  type WriteFailedKind,
} from "../../lib/tauri/writeFailed";

/** Long enough to read one sentence. */
const AUTO_HIDE_MS = 6_000;

interface Shown {
  kind: WriteFailedKind;
  /** Distinguishes one failure from the next, so a repeat re-shows. */
  key: number;
}

/**
 * Says that a like or an addition to a playlist was not saved. The heart
 * and the playlist stay as they were either way, so without a word the user
 * would take the click for one the app ignored, or count on a change that
 * never landed.
 *
 * Fed by `waveflow:write-failed`, which the like and playlist wrappers
 * dispatch when their command rejects.
 */
export function WriteFailedToast() {
  const { t } = useTranslation();
  const [shown, setShown] = useState<Shown | null>(null);

  useEffect(() => {
    let next = 0;
    const onFailed = (event: Event) => {
      const detail = (event as CustomEvent<WriteFailedDetail>).detail;
      if (!detail) return;
      next += 1;
      setShown({ kind: detail.kind, key: next });
    };
    window.addEventListener(WRITE_FAILED_EVENT, onFailed);
    return () => window.removeEventListener(WRITE_FAILED_EVENT, onFailed);
  }, []);

  useEffect(() => {
    if (shown == null) return;
    const timer = window.setTimeout(() => setShown(null), AUTO_HIDE_MS);
    return () => window.clearTimeout(timer);
  }, [shown]);

  // The live region stays mounted, empty between failures: a screen reader
  // announces what is put into a region it already knows, and may say
  // nothing of one inserted with its text. The key sits on the message, so
  // a repeat replaces it inside the same region.
  return createPortal(
    <div
      role="status"
      aria-live="polite"
      className="fixed bottom-52 left-1/2 -translate-x-1/2 z-100"
    >
      {shown != null && (
        <div
          key={shown.key}
          className="flex items-center gap-2 rounded-full border border-zinc-200 bg-white/95 px-4 py-2 text-sm text-zinc-700 shadow-xl backdrop-blur animate-fade-in dark:border-zinc-700 dark:bg-zinc-900/95 dark:text-zinc-200"
        >
          <AlertTriangle
            size={16}
            className="shrink-0 text-amber-500"
            aria-hidden="true"
          />
          <span>
            {t(
              shown.kind === "like"
                ? "writeFailed.like"
                : "writeFailed.playlist",
            )}
          </span>
        </div>
      )}
    </div>,
    document.body,
  );
}

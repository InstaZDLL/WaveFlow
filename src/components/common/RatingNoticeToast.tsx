import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { AlertTriangle } from "lucide-react";

import { RATING_NOT_IN_FILE_EVENT } from "../../lib/tauri/track";

/** Long enough to read one sentence. */
const AUTO_HIDE_MS = 6_000;

/**
 * Says that a rating stayed in the library because the track's file could
 * not take it: a container with no writable tag block, a file another
 * program holds. The stars look the same either way, so without a word the
 * user would count on them travelling with the file.
 *
 * Fed by `waveflow:rating-not-in-file`, which `setTrackRating` dispatches,
 * so every place that rates a track is covered without each of them knowing.
 */
export function RatingNoticeToast() {
  const { t } = useTranslation();
  const [shown, setShown] = useState(0);

  useEffect(() => {
    const onNotice = () => setShown((key) => key + 1);
    window.addEventListener(RATING_NOT_IN_FILE_EVENT, onNotice);
    return () => window.removeEventListener(RATING_NOT_IN_FILE_EVENT, onNotice);
  }, []);

  useEffect(() => {
    if (shown === 0) return;
    const timer = window.setTimeout(() => setShown(0), AUTO_HIDE_MS);
    return () => window.clearTimeout(timer);
  }, [shown]);

  if (shown === 0) return null;

  return createPortal(
    <div
      key={shown}
      role="status"
      aria-live="polite"
      className="fixed bottom-52 left-1/2 -translate-x-1/2 z-100 flex items-center gap-2 rounded-full border border-zinc-200 bg-white/95 px-4 py-2 text-sm text-zinc-700 shadow-xl backdrop-blur animate-fade-in dark:border-zinc-700 dark:bg-zinc-900/95 dark:text-zinc-200"
    >
      <AlertTriangle
        size={16}
        className="shrink-0 text-amber-500"
        aria-hidden="true"
      />
      <span>{t("ratingNotice.notInFile")}</span>
    </div>,
    document.body,
  );
}

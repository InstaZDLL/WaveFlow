import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { ListEnd, ListPlus } from "lucide-react";

import {
  QUEUE_ADDED_EVENT,
  type QueueAddedDetail,
} from "../../lib/tauri/player";

/** Long enough to read, short enough not to sit over the list. */
const AUTO_HIDE_MS = 2_500;

interface Shown extends QueueAddedDetail {
  /** Distinguishes one confirmation from the next, so a repeat re-shows. */
  key: number;
}

/**
 * Confirms "Play next" and "Add to queue". Both act on a queue that is
 * usually out of sight, so without a word back the user could not tell
 * the click had done anything — and tried again, queueing the tracks
 * twice.
 *
 * Fed by `waveflow:queue-added`, which `playerPlayNext` /
 * `playerAddToQueue` dispatch once the backend accepted the tracks: every
 * place that queues tracks is covered without each of them knowing. Sits
 * above the selection bar, which most multi-track queueing comes from.
 */
export function QueueToast() {
  const { t } = useTranslation();
  const [shown, setShown] = useState<Shown | null>(null);

  useEffect(() => {
    let next = 0;
    const onQueued = (event: Event) => {
      const detail = (event as CustomEvent<QueueAddedDetail>).detail;
      if (!detail || detail.count <= 0) return;
      next += 1;
      setShown({ ...detail, key: next });
    };
    window.addEventListener(QUEUE_ADDED_EVENT, onQueued);
    return () => window.removeEventListener(QUEUE_ADDED_EVENT, onQueued);
  }, []);

  useEffect(() => {
    if (shown == null) return;
    const timer = window.setTimeout(() => setShown(null), AUTO_HIDE_MS);
    return () => window.clearTimeout(timer);
  }, [shown]);

  if (shown == null) return null;

  const Icon = shown.mode === "next" ? ListEnd : ListPlus;
  return createPortal(
    <div
      key={shown.key}
      role="status"
      aria-live="polite"
      className="fixed bottom-52 left-1/2 -translate-x-1/2 z-100 flex items-center gap-2 rounded-full border border-zinc-200 bg-white/95 px-4 py-2 text-sm text-zinc-700 shadow-xl backdrop-blur animate-fade-in dark:border-zinc-700 dark:bg-zinc-900/95 dark:text-zinc-200"
    >
      <Icon
        size={16}
        className="shrink-0 text-emerald-500"
        aria-hidden="true"
      />
      <span>
        {t(shown.mode === "next" ? "queueToast.playNext" : "queueToast.added", {
          count: shown.count,
        })}
      </span>
    </div>,
    document.body,
  );
}

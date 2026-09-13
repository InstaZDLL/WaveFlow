import { useEffect } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { AlertTriangle, Info, X } from "lucide-react";

import { usePlayer } from "../../hooks/usePlayer";

/**
 * The kinds each register knows how to put into words. Anything else —
 * an older or newer backend than this build — falls back to the generic
 * sentence rather than rendering a raw key.
 */
const ERROR_KINDS = new Set([
  "device-lost",
  "track-failed",
  "stream-failed",
  "decoder-crashed",
  "decoder-stopped",
  "offline",
]);
const NOTICE_KINDS = new Set([
  "exclusive-refused",
  "dop-refused",
  "paused-device-lost",
]);

/** A fault stays up longer than a piece of information. */
const AUTO_HIDE_ERROR_MS = 12_000;
const AUTO_HIDE_NOTICE_MS = 8_000;

/**
 * Says out loud what playback just had to do (#597).
 *
 * Two registers, deliberately different. A **fault** — the device went
 * away, the file would not open, the decoder crashed — is amber and says
 * what happened; the backend's own message is technical and rides in the
 * tooltip, where a bug report can find it, rather than on screen. A
 * **notice** — exclusive refused, DoP refused, playback parked because
 * the device went away — is neutral: nothing failed, playback is simply
 * not what was asked for, and the engine sends those once per transition
 * so a DAC that never accepts exclusive says so once instead of at every
 * track.
 *
 * Mounted once in AppLayout; the state lives in `PlayerContext`, which
 * owns the two listeners.
 *
 * Sits above the scan toast's slot rather than in it: a scan running
 * while a device disappears is unlikely but not impossible, and two
 * cards in the same corner would cover each other.
 */
export function PlaybackAlertToast() {
  const { t } = useTranslation();
  const { playbackAlert, dismissPlaybackAlert } = usePlayer();

  // Keyed on the alert object, whose identity changes with every
  // occurrence — including a repeat of the same kind, which restarts the
  // countdown rather than letting the first one's timer close the second.
  useEffect(() => {
    if (playbackAlert == null) return;
    const timer = window.setTimeout(
      dismissPlaybackAlert,
      playbackAlert.severity === "error"
        ? AUTO_HIDE_ERROR_MS
        : AUTO_HIDE_NOTICE_MS,
    );
    return () => window.clearTimeout(timer);
  }, [playbackAlert, dismissPlaybackAlert]);

  if (playbackAlert == null) return null;

  const { severity, kind, detail } = playbackAlert;
  const isError = severity === "error";
  const known = (isError ? ERROR_KINDS : NOTICE_KINDS).has(kind);
  const message = t(
    `player.alert.${isError ? "error" : "notice"}.${known ? kind : "unknown"}`,
  );

  // Portalled, not merely z-100: the toast mounts inside the skin's
  // motion wrapper, and any `transform` or `backdrop-filter` ancestor
  // caps the stacking context its z-index is measured in — so a panel
  // far below it in the layer scale can still cover it. See the overlay
  // invariant in CLAUDE.md.
  return createPortal(
    <div
      role={isError ? "alert" : "status"}
      aria-live={isError ? "assertive" : "polite"}
      className={`fixed bottom-44 right-6 z-100 w-80 rounded-2xl border bg-white/95 dark:bg-zinc-900/95 backdrop-blur shadow-xl p-4 animate-fade-in ${
        isError
          ? "border-amber-300 dark:border-amber-500/40"
          : "border-zinc-200 dark:border-zinc-700"
      }`}
    >
      <div className="flex items-start gap-3">
        <div
          className={`shrink-0 w-9 h-9 rounded-full flex items-center justify-center ${
            isError
              ? "bg-amber-500/15 text-amber-500"
              : "bg-zinc-500/10 text-zinc-500 dark:text-zinc-400"
          }`}
        >
          {isError ? <AlertTriangle size={18} /> : <Info size={18} />}
        </div>
        <div className="flex-1 min-w-0">
          <div
            className={`text-sm ${
              isError
                ? "font-semibold text-amber-700 dark:text-amber-500"
                : "text-zinc-700 dark:text-zinc-200"
            }`}
            // The technical detail, on demand and never in the layout.
            title={detail}
          >
            {message}
          </div>
        </div>
        <button
          type="button"
          onClick={dismissPlaybackAlert}
          aria-label={t("player.alert.dismiss")}
          className="shrink-0 p-1 rounded-lg text-zinc-400 hover:text-zinc-600 hover:bg-zinc-100 dark:hover:text-zinc-200 dark:hover:bg-zinc-800 transition-colors"
        >
          <X size={16} />
        </button>
      </div>
    </div>,
    document.body,
  );
}

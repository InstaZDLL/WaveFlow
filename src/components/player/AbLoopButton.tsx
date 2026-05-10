import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Repeat2 } from "lucide-react";
import { listen } from "@tauri-apps/api/event";
import { usePlayer } from "../../hooks/usePlayer";
import {
  playerClearAbLoop,
  playerGetAbLoop,
  playerSetAbLoop,
  type AbLoopSnapshot,
} from "../../lib/tauri/player";

/**
 * Musicolet-style A-B repeat button. Three states cycled by clicking:
 *
 *   idle (no endpoints)  → click captures **A** at the current playhead
 *   A captured           → click captures **B** at the current playhead
 *                           (loop arms automatically when B > A)
 *   A and B captured     → click clears both endpoints
 *
 * State is hydrated from the backend on mount and re-synced on every
 * `player:ab-loop` event so the button stays consistent across views.
 */
export function AbLoopButton() {
  const { t } = useTranslation();
  const { positionMs, currentTrack } = usePlayer();
  const [snap, setSnap] = useState<AbLoopSnapshot>({ a_ms: null, b_ms: null });

  useEffect(() => {
    let cancelled = false;
    playerGetAbLoop()
      .then((s) => {
        if (!cancelled) setSnap(s);
      })
      .catch(() => {});
    const unlisten = listen<AbLoopSnapshot>("player:ab-loop", (e) => {
      setSnap(e.payload);
    });
    return () => {
      cancelled = true;
      unlisten.then((fn) => fn()).catch(() => {});
    };
  }, []);

  const hasA = snap.a_ms != null;
  const hasB = snap.b_ms != null;
  const armed = hasA && hasB;

  const handleClick = async () => {
    try {
      if (armed) {
        const next = await playerClearAbLoop();
        setSnap(next);
        return;
      }
      // Use position - 1 ms to avoid the loop firing on the same
      // sample we just captured B at (would cause an immediate seek).
      const ms = Math.max(0, Math.floor(positionMs));
      if (!hasA) {
        const next = await playerSetAbLoop(ms, null);
        setSnap(next);
      } else {
        // Capturing B: ensure it's strictly greater than A; if not,
        // swap so the user gets a usable loop instead of a no-op.
        const a = snap.a_ms ?? 0;
        const next = ms > a
          ? await playerSetAbLoop(null, ms)
          : await playerSetAbLoop(ms, a);
        setSnap(next);
      }
    } catch (err) {
      console.error("[AbLoopButton] toggle failed", err);
    }
  };

  const label = armed
    ? t("abLoop.armed", {
        a: formatMs(snap.a_ms ?? 0),
        b: formatMs(snap.b_ms ?? 0),
      })
    : hasA
      ? t("abLoop.setB", { a: formatMs(snap.a_ms ?? 0) })
      : t("abLoop.setA");

  // Tri-state colour: zinc (idle), amber (only A captured, awaiting B),
  // emerald (loop active). Mirrors the Lyrics / Sleep timer accent.
  const tone = armed
    ? "text-emerald-500"
    : hasA
      ? "text-amber-500"
      : "text-zinc-400 hover:text-zinc-800 dark:hover:text-white";

  return (
    <button
      type="button"
      onClick={handleClick}
      aria-label={label}
      title={label}
      disabled={currentTrack == null}
      className={`relative p-2 rounded-lg transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${tone}`}
    >
      <Repeat2 size={20} />
      {(hasA || hasB) && (
        <span className="absolute -top-1 -right-1 text-[9px] font-bold leading-none px-1 py-[2px] rounded bg-current text-white dark:text-zinc-900">
          {armed ? "AB" : "A"}
        </span>
      )}
    </button>
  );
}

function formatMs(ms: number): string {
  const s = Math.floor(ms / 1000);
  const mm = Math.floor(s / 60);
  const ss = (s % 60).toString().padStart(2, "0");
  return `${mm}:${ss}`;
}

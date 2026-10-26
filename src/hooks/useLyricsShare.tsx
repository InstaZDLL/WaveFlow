import { useState } from "react";

import { LyricsShareModal } from "../components/common/LyricsShareModal";
import type { Track } from "../lib/tauri/track";

interface Snapshot {
  track: Track;
  lines: string[];
  from: number;
  /** A new dialog per opening, so it reads its first selection afresh. */
  seq: number;
}

/**
 * The lyrics excerpt dialog for a surface that shows lyrics: the lyrics
 * panel and the immersive view's share menu.
 *
 * The track and its lines are taken when the dialog opens and held until
 * it closes. The selection is a set of line indices, and a track that
 * changes under an open dialog would otherwise point them at another
 * song's lines. The snapshot outlives the closing so the dialog can
 * animate out with what it showed.
 */
export function useLyricsShare() {
  const [open, setOpen] = useState(false);
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);

  const openShare = (track: Track, lines: string[], from: number) => {
    setSnapshot((prev) => ({ track, lines, from, seq: (prev?.seq ?? 0) + 1 }));
    setOpen(true);
  };

  const render = () =>
    snapshot && (
      <LyricsShareModal
        key={snapshot.seq}
        isOpen={open}
        onClose={() => setOpen(false)}
        track={snapshot.track}
        lines={snapshot.lines}
        initialIndex={snapshot.from}
      />
    );

  return { open: openShare, render };
}

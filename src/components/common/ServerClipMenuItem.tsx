import { useTranslation } from "react-i18next";
import { Clapperboard } from "lucide-react";

import { usePlayer } from "../../hooks/usePlayer";
import type { RemoteContextTrack } from "../../hooks/useRemoteTrackContextMenu";
import { useServerTrackClip } from "../../hooks/useTrackClip";
import { openClipForTrack } from "../../lib/clipPlayer";
import { ContextMenuItem } from "./ContextMenu";

/**
 * The track's music video (RFC-029), when the mirror says the server has one
 * or the local file it was reconciled with does. Read from the mirror as the
 * menu opens, never asked of the server: the entry appears once the answer
 * is in, and only when there is a clip.
 */
export function ServerClipMenuItem({
  track,
  onDone,
}: {
  track: RemoteContextTrack;
  onDone: () => void;
}) {
  const { t } = useTranslation();
  const { openRightPanel, currentTrack } = usePlayer();
  const clip = useServerTrackClip(track.remoteId);
  if (!clip) return null;
  return (
    <ContextMenuItem
      icon={<Clapperboard size={16} aria-hidden="true" />}
      label={t("clip.play")}
      onSelect={() => {
        openClipForTrack(
          {
            // Never the playing track's sentinel: the server's id is what
            // tells the two apart.
            id: Number.NaN,
            remote_id: track.remoteId,
            title: track.title,
            artist_name: track.artist,
          },
          clip,
          currentTrack,
        );
        // The clip shows in the now-playing panel, enlarged from there.
        openRightPanel("nowPlaying");
        onDone();
      }}
    />
  );
}

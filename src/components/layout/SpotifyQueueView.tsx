import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ListMusic } from "lucide-react";
import { Artwork } from "../common/Artwork";
import { usePlayer } from "../../hooks/usePlayer";
import {
  spotifyGetQueue,
  type SpotifyQueueSnapshot,
  type SpotifyTrackLite,
} from "../../lib/tauri/spotify";

/**
 * Read-only Spotify queue. Spotify exposes `GET /me/player/queue`
 * but no public API to reorder, jump-by-index, or remove items —
 * those actions stay on the Spotify client side. So we render a
 * stripped-down list (now playing + ~20 upcoming) without the
 * drag-handles or double-click-to-jump that the local QueuePanel
 * has.
 *
 * Refresh strategy: re-fetch when the panel mounts and on every
 * track change so a "play next" added from the Spotify mobile / web
 * client appears in the WaveFlow widget within one track.
 */
export function SpotifyQueueView() {
  const { t } = useTranslation();
  const { currentTrack } = usePlayer();
  const [snapshot, setSnapshot] = useState<SpotifyQueueSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    spotifyGetQueue()
      .then((q) => {
        if (cancelled) return;
        setSnapshot(q);
        setError(null);
      })
      .catch((err) => {
        if (cancelled) return;
        console.error("[SpotifyQueueView] fetch failed", err);
        setError(String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [currentTrack?.id]);

  const upcoming = snapshot?.upcoming ?? [];
  const total = (snapshot?.current ? 1 : 0) + upcoming.length;

  if (error) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center text-center px-4">
        <p className="text-sm text-red-500 max-w-60">{error}</p>
      </div>
    );
  }

  if (total === 0) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center text-center">
        <div className="w-24 h-24 bg-zinc-100 dark:bg-zinc-800 rounded-2xl flex items-center justify-center mb-6 shadow-inner">
          <ListMusic
            size={40}
            className="text-zinc-300 dark:text-zinc-600"
            aria-hidden="true"
          />
        </div>
        <h3 className="font-semibold mb-2">{t("queue.emptyTitle")}</h3>
        <p className="text-sm text-zinc-500 max-w-50">
          {t("spotifyQueue.emptyHint")}
        </p>
      </div>
    );
  }

  return (
    <div className="flex-1 overflow-y-auto -mx-2 px-2 space-y-5 scrollbar-hide">
      {snapshot?.current && (
        <section>
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-2 px-1">
            {t("queue.nowPlaying")}
          </div>
          <SpotifyQueueRow track={snapshot.current} isCurrent />
        </section>
      )}
      {upcoming.length > 0 && (
        <section>
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-2 px-1">
            {t("queue.upNext", { count: upcoming.length })}
          </div>
          <ul className="space-y-1">
            {upcoming.map((track, idx) => (
              <li key={`${track.id ?? track.uri}-${idx}`}>
                <SpotifyQueueRow track={track} />
              </li>
            ))}
          </ul>
        </section>
      )}
      <p className="text-[10px] text-zinc-400 text-center px-2">
        {t("spotifyQueue.readonlyHint")}
      </p>
    </div>
  );
}

function SpotifyQueueRow({
  track,
  isCurrent = false,
}: {
  track: SpotifyTrackLite;
  isCurrent?: boolean;
}) {
  return (
    <div
      className={`flex items-center space-x-3 p-2 rounded-lg transition-colors select-none ${
        isCurrent
          ? "bg-emerald-50 dark:bg-emerald-900/20"
          : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
      }`}
    >
      <Artwork
        path={track.image_url}
        size="full"
        className="w-10 h-10"
        iconSize={18}
        alt={track.album_name ?? track.name}
        rounded="md"
      />
      <div className="flex-1 min-w-0">
        <div
          className={`text-sm truncate ${
            isCurrent
              ? "text-emerald-600 dark:text-emerald-400 font-semibold"
              : "text-zinc-800 dark:text-zinc-200"
          }`}
        >
          {track.name}
        </div>
        <div className="text-xs text-zinc-500 truncate">
          {track.artist_name ?? "—"}
        </div>
      </div>
    </div>
  );
}

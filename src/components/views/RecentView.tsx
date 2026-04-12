import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Clock } from "lucide-react";
import { EmptyState } from "../common/EmptyState";
import { Artwork } from "../common/Artwork";

import { usePlayer } from "../../hooks/usePlayer";
import { listRecentPlays, type RecentPlay } from "../../lib/tauri/browse";
import { formatDuration } from "../../lib/tauri/track";

const LIMIT = 50;

/**
 * Format a unix-millis timestamp as a relative "X minutes ago" /
 * "hier" / absolute date string. Kept inline so it's easy to
 * localize later without round-tripping the backend.
 */
function formatPlayedAt(ts: number, locale: string): string {
  const now = Date.now();
  const deltaSec = Math.max(0, Math.floor((now - ts) / 1000));
  if (deltaSec < 60) return locale === "fr" ? "à l'instant" : "just now";
  const deltaMin = Math.floor(deltaSec / 60);
  if (deltaMin < 60)
    return locale === "fr" ? `il y a ${deltaMin} min` : `${deltaMin} min ago`;
  const deltaHour = Math.floor(deltaMin / 60);
  if (deltaHour < 24)
    return locale === "fr" ? `il y a ${deltaHour} h` : `${deltaHour} h ago`;
  const deltaDay = Math.floor(deltaHour / 24);
  if (deltaDay === 1) return locale === "fr" ? "hier" : "yesterday";
  if (deltaDay < 7)
    return locale === "fr" ? `il y a ${deltaDay} jours` : `${deltaDay} days ago`;
  return new Date(ts).toLocaleDateString();
}

export function RecentView() {
  const { t, i18n } = useTranslation();
  const { playbackState, currentTrack } = usePlayer();
  const [tracks, setTracks] = useState<RecentPlay[]>([]);
  const [isLoading, setIsLoading] = useState(false);

  // Reload on library change + whenever playback transitions to a
  // new "ended" state (which means a play_event row has just been
  // written by the analytics task).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setIsLoading(true);
      try {
        // Pass null to get recent plays across all libraries.
        const list = await listRecentPlays(null, LIMIT);
        if (!cancelled) setTracks(list);
      } catch (err) {
        if (!cancelled) {
          console.error("[RecentView] failed to load recent plays", err);
          setTracks([]);
        }
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
    // The `playbackState === "ended"` re-fetch captures auto-advance
    // naturally — when a track finishes, play_event is inserted
    // before the next LoadAndPlay fires.
  }, [playbackState]);

  return (
    <div className="max-w-6xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Header */}
      <div className="flex items-center space-x-6">
        <div className="w-24 h-24 rounded-2xl bg-blue-100 text-blue-500 flex items-center justify-center shadow-sm dark:bg-blue-950/60 dark:text-blue-400">
          <Clock size={48} />
        </div>
        <div>
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
            {t("recent.badge")}
          </div>
          <h1 className="text-4xl font-bold text-zinc-900 dark:text-white">
            {t("recent.title")}
          </h1>
          <div className="text-sm text-zinc-500 mt-1">
            {t("recent.count", { count: tracks.length })}
          </div>
        </div>
      </div>

      {tracks.length === 0 ? (
        <EmptyState
          icon={<Clock size={40} />}
          title={t("recent.emptyTitle")}
          description={t("recent.emptyDescription")}
          accent="blue"
          shape="circle"
          className="py-20"
        />
      ) : (
        <div
          className={`rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 overflow-hidden ${
            isLoading ? "opacity-50" : ""
          }`}
        >
          <ul className="divide-y divide-zinc-100 dark:divide-zinc-800/60">
            {tracks.map((track) => {
              const isCurrent = track.track_id === currentTrack?.id;
              return (
                <li
                  key={track.track_id}
                  className={`grid grid-cols-[3rem_1fr_1fr_8rem_5rem] gap-4 px-5 py-2 items-center transition-colors ${
                    isCurrent
                      ? "bg-emerald-50 dark:bg-emerald-900/20"
                      : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
                  }`}
                >
                  <Artwork
                    path={track.artwork_path}
                    className="w-10 h-10"
                    iconSize={18}
                    alt={track.album_title ?? track.title}
                    rounded="md"
                  />
                  <div className="min-w-0">
                    <div
                      className={`text-sm truncate ${
                        isCurrent
                          ? "text-emerald-600 dark:text-emerald-400 font-semibold"
                          : "text-zinc-800 dark:text-zinc-200"
                      }`}
                    >
                      {track.title}
                    </div>
                    <div className="text-xs text-zinc-500 truncate">
                      {track.artist_name ?? "—"}
                    </div>
                  </div>
                  <span className="text-sm text-zinc-500 truncate">
                    {track.album_title ?? "—"}
                  </span>
                  <span className="text-xs text-zinc-400 text-right">
                    {formatPlayedAt(track.played_at, i18n.language)}
                  </span>
                  <span className="text-sm tabular-nums text-zinc-400 text-right">
                    {formatDuration(track.duration_ms)}
                  </span>
                </li>
              );
            })}
          </ul>
        </div>
      )}
    </div>
  );
}

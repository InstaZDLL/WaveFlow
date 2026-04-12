import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Heart, Clock, Play } from "lucide-react";
import { EmptyState } from "../common/EmptyState";
import { Artwork } from "../common/Artwork";
import { usePlayer } from "../../hooks/usePlayer";
import {
  listLikedTracks,
  toggleLikeTrack,
  formatDuration,
  type Track,
} from "../../lib/tauri/track";

export function LikedView() {
  const { t } = useTranslation();
  const { playTracks, currentTrack, playbackState } = usePlayer();
  const [tracks, setTracks] = useState<Track[]>([]);
  const [isLoading, setIsLoading] = useState(false);

  // Reload when the view mounts and when playback ends (a new
  // play_event might bump the sidebar counter — keep in sync).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setIsLoading(true);
      try {
        const list = await listLikedTracks();
        if (!cancelled) setTracks(list);
      } catch (err) {
        if (!cancelled) {
          console.error("[LikedView] failed to load liked tracks", err);
        }
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [playbackState]);

  const handleUnlike = async (trackId: number) => {
    await toggleLikeTrack(trackId);
    setTracks((prev) => prev.filter((t) => t.id !== trackId));
  };

  const handlePlayAll = () => {
    if (tracks.length > 0) {
      playTracks(tracks, 0, { type: "liked", id: null });
    }
  };

  const unknown = t("library.table.unknown");

  return (
    <div className="max-w-6xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Header */}
      <div className="flex items-start justify-between">
        <div className="flex items-center space-x-6">
          <div className="w-24 h-24 rounded-2xl bg-pink-100 text-pink-500 flex items-center justify-center shadow-sm dark:bg-pink-950/60 dark:text-pink-400">
            <Heart size={48} className="fill-current" />
          </div>
          <div>
            <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
              {t("liked.badge")}
            </div>
            <h1 className="text-4xl font-bold text-zinc-900 dark:text-white">
              {t("liked.title")}
            </h1>
            <div className="text-sm text-zinc-500 mt-1">
              {t("liked.count", { count: tracks.length })}
            </div>
          </div>
        </div>

        {tracks.length > 0 && (
          <button
            type="button"
            onClick={handlePlayAll}
            className="bg-pink-500 hover:bg-pink-600 text-white px-4 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm shadow-pink-500/30"
          >
            <Play size={16} className="fill-current" />
            <span>{t("liked.playAll")}</span>
          </button>
        )}
      </div>

      {/* Tracks */}
      {tracks.length > 0 ? (
        <div className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 overflow-hidden">
          <div className="grid grid-cols-[3rem_2.75rem_1fr_1fr_1fr_5rem_2.5rem] gap-4 px-5 py-3 text-[10px] font-bold tracking-widest text-zinc-400 uppercase border-b border-zinc-100 dark:border-zinc-800">
            <span className="text-right">{t("library.table.number")}</span>
            <span aria-hidden="true" />
            <span>{t("library.table.title")}</span>
            <span>{t("library.table.artist")}</span>
            <span>{t("library.table.album")}</span>
            <span className="flex justify-end" aria-label={t("library.table.duration")}>
              <Clock size={14} />
            </span>
            <span aria-hidden="true" />
          </div>
          <ul
            className={`divide-y divide-zinc-100 dark:divide-zinc-800/60 ${
              isLoading ? "opacity-50" : ""
            }`}
          >
            {tracks.map((track, index) => {
              const isCurrent = track.id === currentTrack?.id;
              return (
                <li
                  key={track.id}
                  onDoubleClick={() =>
                    playTracks(tracks, index, { type: "liked", id: null })
                  }
                  className={`group grid grid-cols-[3rem_2.75rem_1fr_1fr_1fr_5rem_2.5rem] gap-4 px-5 py-2 items-center select-none transition-colors cursor-pointer ${
                    isCurrent
                      ? "bg-emerald-50 dark:bg-emerald-900/20"
                      : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
                  }`}
                >
                  <span
                    className={`text-right text-sm tabular-nums ${
                      isCurrent ? "text-emerald-500 font-semibold" : "text-zinc-400"
                    }`}
                  >
                    {index + 1}
                  </span>
                  <Artwork
                    path={track.artwork_path}
                    className="w-10 h-10"
                    iconSize={18}
                    alt={track.album_title ?? track.title}
                    rounded="md"
                  />
                  <span
                    className={`text-sm truncate ${
                      isCurrent
                        ? "text-emerald-600 dark:text-emerald-400 font-semibold"
                        : "text-zinc-800 dark:text-zinc-200"
                    }`}
                  >
                    {track.title}
                  </span>
                  <span className="text-sm text-zinc-500 truncate">
                    {track.artist_name ?? unknown}
                  </span>
                  <span className="text-sm text-zinc-500 truncate">
                    {track.album_title ?? unknown}
                  </span>
                  <span className="text-sm tabular-nums text-zinc-400 text-right">
                    {formatDuration(track.duration_ms)}
                  </span>
                  <div className="flex justify-center">
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        handleUnlike(track.id);
                      }}
                      aria-label={t("liked.unlike")}
                      className="p-1.5 rounded-full text-pink-500 hover:text-pink-600 hover:bg-pink-50 dark:hover:bg-pink-500/10 transition-colors opacity-0 group-hover:opacity-100"
                    >
                      <Heart size={16} className="fill-current" />
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        </div>
      ) : (
        <EmptyState
          icon={<Heart size={40} className="fill-current" />}
          title={t("liked.emptyTitle")}
          description={t("liked.emptyDescription")}
          accent="pink"
          shape="circle"
          className="py-20"
        />
      )}
    </div>
  );
}

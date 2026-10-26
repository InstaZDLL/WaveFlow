import { ArrowUpRight, Loader2, RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Artwork } from "../../../common/Artwork";
import type { Playlist } from "../../../../lib/tauri/playlist";
import {
  smartFamilyStyle,
  smartPlaylistKind,
} from "../../../../lib/tauri/playlist";
import type { TopAlbumRow } from "../../../../lib/tauri/stats";
import { resolveRemoteImage } from "../../../../lib/tauri/artwork";
import { SectionHeader } from "./SectionHeader";
import { useAlbumArtistContextMenu } from "../../../../hooks/useAlbumArtistContextMenu";

interface ForYouMosaicProps {
  featured: TopAlbumRow | null;
  smartPlaylists: Playlist[];
  isRegenerating: boolean;
  onRegenerate: () => void;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToPlaylist: (playlistId: number) => void;
}

/**
 * "Pour vous" as an editorial mosaic instead of a row of identical
 * 16:7 banners. One featured record (the most played, all time) takes
 * half the width with magazine-sized type over its own artwork; two
 * Daily Mix cards stand beside it in portrait with their composite
 * covers; every other mix becomes a compact chip — the place where
 * density helps.
 */
export function ForYouMosaic({
  featured,
  smartPlaylists,
  isRegenerating,
  onRegenerate,
  onNavigateToAlbum,
  onNavigateToPlaylist,
}: ForYouMosaicProps) {
  const { t } = useTranslation();
  const daily = smartPlaylists
    .filter((p) => smartPlaylistKind(p)?.kind === "daily_mix")
    .slice(0, 2);
  const rest = smartPlaylists.filter((p) => !daily.includes(p));
  // Right-click on the featured album: pin it to the sidebar.
  const albumMenu = useAlbumArtistContextMenu();
  if (!featured && smartPlaylists.length === 0) return null;

  return (
    <section aria-labelledby="fe-foryou-title">
      <SectionHeader
        title={t("home.dailyMix.title", "Pour vous")}
        className="mb-5"
        action={
          <button
            type="button"
            onClick={onRegenerate}
            disabled={isRegenerating}
            className="fe-link inline-flex items-center gap-2 text-[0.8125rem] disabled:opacity-50"
          >
            {isRegenerating ? (
              <Loader2 size={13} className="animate-spin" />
            ) : (
              <RefreshCw size={13} strokeWidth={1.75} />
            )}
            {isRegenerating
              ? t("home.dailyMix.regenerating", "Génération…")
              : t("home.dailyMix.regenerate", "Régénérer")}
          </button>
        }
      />
      <span id="fe-foryou-title" className="sr-only">
        {t("home.dailyMix.title", "Pour vous")}
      </span>

      <div className="grid grid-cols-12 gap-4 lg:auto-rows-[10.75rem]">
        {featured && (
          <button
            type="button"
            onClick={() => onNavigateToAlbum(featured.album_id)}
            {...albumMenu.bind({ kind: "album", id: featured.album_id })}
            className="fe-tile-hover group col-span-12 row-span-2 text-left lg:col-span-6"
          >
            <div className="fe-tile relative h-full min-h-[18rem] w-full rounded-[1.125rem]!">
              <Artwork
                path={featured.artwork_path}
                path1x={featured.artwork_path_1x}
                path2x={featured.artwork_path_2x}
                size="full"
                alt=""
                className="absolute inset-0 h-full w-full rounded-none!"
                iconSize={48}
                rounded="md"
              />
              <div
                className="absolute inset-0"
                style={{
                  background:
                    "linear-gradient(180deg, rgb(11 11 14 / 0.05) 0%, rgb(11 11 14 / 0.25) 40%, rgb(11 11 14 / 0.9) 100%)",
                }}
              />
              <div className="absolute inset-x-0 bottom-0 flex items-end justify-between gap-6 p-6">
                <div className="min-w-0">
                  <p className="fe-editorial text-[1rem] text-white/80">
                    {t("home.fluid.featuredKicker")}
                  </p>
                  <p className="fe-display mt-2 truncate text-[clamp(1.5rem,2.2vw,2.125rem)] text-white">
                    {featured.title}
                  </p>
                  <p className="mt-1.5 text-[0.875rem] text-white/75">
                    {featured.artist_name ?? t("library.table.unknown")}
                    <span className="mx-2 opacity-50">·</span>
                    <span className="fe-mono">
                      {t("home.fluid.plays", { count: featured.plays })}
                    </span>
                  </p>
                </div>
                <span className="fe-press inline-flex h-10 shrink-0 items-center gap-2 rounded-full bg-white pl-4 pr-3 text-[0.8125rem] font-medium text-[#0b0b0e] transition-transform group-hover:scale-[1.03]">
                  {t("home.fluid.openAlbum")}
                  <ArrowUpRight size={14} strokeWidth={2} />
                </span>
              </div>
            </div>
          </button>
        )}

        {daily.map((p) => (
          <MixCard key={p.id} playlist={p} onOpen={onNavigateToPlaylist} />
        ))}
      </div>

      {rest.length > 0 && (
        <div className="mt-5">
          <p className="fe-label mb-2.5">{t("home.fluid.yourMixes")}</p>
          <ul className="flex flex-wrap gap-2.5">
            {rest.map((p) => {
              const cover = resolveRemoteImage(p.cover_path, null);
              const style = smartFamilyStyle(smartPlaylistKind(p));
              return (
                <li key={p.id}>
                  <button
                    type="button"
                    onClick={() => onNavigateToPlaylist(p.id)}
                    className="fe-press flex items-center gap-2.5 rounded-full py-1 pl-1 pr-3.5 ring-1 ring-[var(--fe-hairline-strong)] hover:bg-[var(--fe-tone-1)] hover:ring-[var(--fe-hairline-strong)]"
                  >
                    {cover ? (
                      <img
                        src={cover}
                        alt=""
                        loading="lazy"
                        className="h-7 w-7 rounded-full object-cover"
                      />
                    ) : (
                      <span
                        className={`h-7 w-7 rounded-full ${style.gradient}`}
                      />
                    )}
                    <span className="fe-ink text-[0.8125rem] font-medium">
                      {p.name}
                    </span>
                    <span className="fe-ink-3 fe-mono text-[0.75rem]">
                      {p.track_count}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        </div>
      )}
      {albumMenu.render()}
    </section>
  );
}

function MixCard({
  playlist,
  onOpen,
}: {
  playlist: Playlist;
  onOpen: (id: number) => void;
}) {
  const { t } = useTranslation();
  const cover = resolveRemoteImage(playlist.cover_path, null);
  const style = smartFamilyStyle(smartPlaylistKind(playlist));
  return (
    <button
      type="button"
      onClick={() => onOpen(playlist.id)}
      className="fe-tile-hover col-span-6 row-span-2 text-left lg:col-span-3"
    >
      <div className="flex h-full flex-col rounded-[1.125rem] bg-[var(--fe-surface)] p-3">
        <div className="fe-tile aspect-square w-full rounded-[0.75rem]!">
          {cover ? (
            <img src={cover} alt="" loading="lazy" decoding="async" />
          ) : (
            <div className={`h-full w-full ${style.gradient}`} />
          )}
        </div>
        <div className="mt-auto px-1 pb-1 pt-4">
          <p className="fe-label">{t(style.labelKey)}</p>
          <p className="fe-title fe-ink mt-1 truncate text-[1.0625rem]">
            {playlist.name}
          </p>
          <p className="fe-ink-3 fe-mono mt-0.5 text-[0.8125rem]">
            {t("home.dailyMix.trackCount", { count: playlist.track_count })}
          </p>
        </div>
      </div>
    </button>
  );
}

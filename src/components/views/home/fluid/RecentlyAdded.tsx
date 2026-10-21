import { useTranslation } from "react-i18next";
import { Artwork } from "../../../common/Artwork";
import type { AlbumRow } from "../../../../lib/tauri/browse";
import { SectionHeader } from "./SectionHeader";

interface RecentlyAddedProps {
  albums: AlbumRow[];
  loading: boolean;
  onNavigateToAlbum: (albumId: number) => void;
  onSeeAll: () => void;
}

/**
 * Square tiles, as before, but the newest record is given four times
 * the area and a fuller caption — one thing on the row has presence,
 * the rest are a shelf. The caption says why: it is the latest
 * addition, not an arbitrary first element.
 */
export function RecentlyAdded({
  albums,
  loading,
  onNavigateToAlbum,
  onSeeAll,
}: RecentlyAddedProps) {
  const { t } = useTranslation();
  if (!loading && albums.length === 0) return null;
  const shown = albums.slice(0, 9);
  const [lead, ...rest] = shown;

  return (
    <section aria-labelledby="fe-added-title">
      <SectionHeader
        title={t("home.recentlyAdded.title")}
        className="mb-5"
        action={
          <button
            type="button"
            className="fe-link text-[0.8125rem]"
            onClick={onSeeAll}
          >
            {t("home.seeAll")}
          </button>
        }
      />
      <span id="fe-added-title" className="sr-only">
        {t("home.recentlyAdded.title")}
      </span>
      {loading && albums.length === 0 ? (
        <div
          role="status"
          aria-busy="true"
          aria-label={t("home.recentlyAdded.title")}
          className="grid animate-pulse grid-cols-3 gap-x-4 gap-y-6 sm:grid-cols-4 lg:grid-cols-6"
        >
          <div className="col-span-2 row-span-2 aspect-square rounded-[0.875rem] bg-[var(--fe-tone-1)]" />
          {Array.from({ length: 8 }).map((_, i) => (
            <div
              key={i}
              className="aspect-square rounded-[0.875rem] bg-[var(--fe-tone-1)]"
            />
          ))}
        </div>
      ) : (
        <div className="grid grid-cols-3 gap-x-4 gap-y-6 sm:grid-cols-4 lg:grid-cols-6">
          {lead && <AlbumTile album={lead} lead onOpen={onNavigateToAlbum} />}
          {rest.map((a) => (
            <AlbumTile key={a.id} album={a} onOpen={onNavigateToAlbum} />
          ))}
        </div>
      )}
    </section>
  );
}

function AlbumTile({
  album,
  lead,
  onOpen,
}: {
  album: AlbumRow;
  lead?: boolean;
  onOpen: (albumId: number) => void;
}) {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      onClick={() => onOpen(album.id)}
      className={`fe-tile-hover text-left ${lead ? "col-span-2 row-span-2" : ""}`}
    >
      <Artwork
        path={album.artwork_path}
        path1x={album.artwork_path_1x}
        path2x={album.artwork_path_2x}
        // The full cover for every tile: the 2× thumbnail is 128 px and
        // a secondary tile runs to 270 px on a wide window, where it
        // read as out of focus beside the lead.
        size="full"
        alt=""
        className="fe-tile aspect-square w-full"
        iconSize={lead ? 48 : 28}
        rounded="xl"
      />
      <div className={`min-w-0 ${lead ? "mt-4" : "mt-2.5"}`}>
        {lead && (
          <p className="fe-label mb-1.5">
            {t("home.fluid.leadKicker")}
            <span className="mx-1.5 opacity-50">·</span>
            {t("sidebar.library.tracksCount", { count: album.track_count })}
          </p>
        )}
        <p
          className={`fe-ink truncate font-medium ${lead ? "fe-title text-[1.25rem]" : "text-[0.8125rem]"}`}
        >
          {album.title}
        </p>
        <p
          className={`fe-ink-3 truncate ${lead ? "mt-1 text-[0.875rem]" : "text-[0.8125rem]"}`}
        >
          {album.artist_name ?? t("library.table.unknown")}
          {lead && album.year && (
            <>
              <span className="mx-2 opacity-50">·</span>
              <span className="fe-mono">{album.year}</span>
            </>
          )}
        </p>
      </div>
    </button>
  );
}

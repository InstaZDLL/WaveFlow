import { fixtures, trackFromAlbum, type FeAlbum } from "../fixtures";
import { fill } from "../copy";
import { integer, relativeTime } from "../format";
import { useFe } from "../useFe";
import { Cover } from "../ui/Cover";
import { SectionHeader } from "../ui/SectionHeader";

/**
 * Square tiles, as before, but the newest record is given four times
 * the area and a fuller caption — one thing on the row has presence,
 * the rest are a shelf. No padding box around each tile: the caption
 * sits straight under the artwork.
 */
export function RecentlyAdded() {
  const { copy } = useFe();
  const albums = fixtures.recentAlbums.slice(0, 9);
  const [lead, ...rest] = albums;

  return (
    <section aria-labelledby="fe-added-title">
      <SectionHeader
        title={copy.recentlyAdded.title}
        className="mb-5"
        action={
          <button type="button" className="fe-link text-[0.8125rem]">
            {copy.seeAll}
          </button>
        }
      />
      <span id="fe-added-title" className="sr-only">
        {copy.recentlyAdded.title}
      </span>
      <div className="grid grid-cols-3 gap-x-4 gap-y-6 sm:grid-cols-4 lg:grid-cols-6">
        {lead && <AlbumTile album={lead} lead />}
        {rest.map((a) => (
          <AlbumTile key={a.id} album={a} />
        ))}
      </div>
    </section>
  );
}

function AlbumTile({ album, lead }: { album: FeAlbum; lead?: boolean }) {
  const { copy, lang, play } = useFe();
  return (
    <button
      type="button"
      onClick={() => play(trackFromAlbum(album))}
      className={`fe-tile-hover text-left ${lead ? "col-span-2 row-span-2" : ""}`}
    >
      <Cover
        src={album.cover}
        palette={album.palette}
        alt={album.title}
        decorative
        className="aspect-square w-full"
      />
      <div className={`min-w-0 ${lead ? "mt-4" : "mt-2.5"}`}>
        {lead && (
          <p className="fe-label mb-1.5">
            {copy.recentlyAdded.leadKicker}
            <span className="mx-1.5 opacity-50">·</span>
            {relativeTime(album.addedAt, lang)}
          </p>
        )}
        <p
          className={`truncate font-medium text-[var(--fe-ink)] ${lead ? "fe-title text-[1.25rem]" : "text-[0.8125rem]"}`}
        >
          {album.title}
        </p>
        <p
          className={`fe-ink-3 truncate ${lead ? "mt-1 text-[0.875rem]" : "text-[0.8125rem]"}`}
        >
          {album.artist}
          {lead && album.year && (
            <>
              <span className="mx-2 opacity-50">·</span>
              <span className="fe-mono">{album.year}</span>
            </>
          )}
          {lead && album.genre && (
            <>
              <span className="mx-2 opacity-50">·</span>
              {album.genre}
            </>
          )}
        </p>
        {lead && (
          <p className="fe-ink-3 fe-mono mt-1 text-[0.8125rem]">
            {fill(copy.sidebar.trackCount, {
              count: integer(album.trackCount, lang),
            })}
          </p>
        )}
      </div>
    </button>
  );
}

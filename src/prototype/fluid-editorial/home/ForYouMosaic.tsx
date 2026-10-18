import { Play, RefreshCw } from "lucide-react";
import { fixtures, trackFromAlbum, type FePlaylist } from "../fixtures";
import { fill } from "../copy";
import { integer } from "../format";
import { useFe } from "../useFe";
import { Cover, Mosaic } from "../ui/Cover";
import { SectionHeader } from "../ui/SectionHeader";

/**
 * "Pour vous" as an editorial mosaic instead of a row of identical
 * 16:7 banners. One featured record (the most played) takes half the
 * width with magazine-sized type over its own artwork; two portrait
 * Daily Mix cards stand beside it with their 2×2 covers; every other
 * mix becomes a compact chip — the place where density helps.
 */
export function ForYouMosaic() {
  const { copy, lang, play } = useFe();
  const featured = fixtures.topAlbums[0];
  const smart = fixtures.playlists.filter((p) => p.isSmart);
  const daily = smart.filter((p) => p.smartKind === "daily_mix").slice(0, 2);
  const rest = smart.filter((p) => !daily.includes(p));

  return (
    <section aria-labelledby="fe-foryou-title">
      <SectionHeader
        title={copy.forYou.title}
        className="mb-5"
        action={
          <button
            type="button"
            className="fe-link inline-flex items-center gap-2 text-[0.8125rem]"
          >
            <RefreshCw size={13} strokeWidth={1.75} />
            {copy.forYou.regenerate}
          </button>
        }
      />
      <span id="fe-foryou-title" className="sr-only">
        {copy.forYou.title}
      </span>

      <div className="grid grid-cols-12 gap-4 lg:auto-rows-[10.75rem]">
        {/* Featured */}
        <button
          type="button"
          onClick={() => play(trackFromAlbum(featured))}
          className="fe-tile-hover group col-span-12 row-span-2 text-left lg:col-span-6"
        >
          <div className="fe-tile relative h-full min-h-[18rem] w-full rounded-[1.125rem]!">
            <Cover
              src={featured.cover}
              palette={featured.palette}
              alt={featured.title}
              decorative
              className="absolute inset-0 h-full w-full rounded-none! shadow-none!"
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
                  {copy.forYou.featuredKicker}
                </p>
                <p className="fe-display mt-2 truncate text-[clamp(1.5rem,2.2vw,2.125rem)] text-white">
                  {featured.title}
                </p>
                <p className="mt-1.5 text-[0.875rem] text-white/75">
                  {featured.artist}
                  <span className="mx-2 opacity-50">·</span>
                  <span className="fe-mono">
                    {fill(copy.forYou.plays, {
                      count: integer(featured.plays, lang),
                    })}
                  </span>
                  {featured.genre && (
                    <>
                      <span className="mx-2 opacity-50">·</span>
                      {featured.genre}
                    </>
                  )}
                </p>
              </div>
              <span className="fe-press inline-flex h-10 shrink-0 items-center gap-2 rounded-full bg-white pl-3 pr-4 text-[0.8125rem] font-medium text-[#0b0b0e] transition-transform group-hover:scale-[1.03]">
                <Play size={13} className="fill-current" strokeWidth={0} />
                {copy.forYou.featuredCta}
              </span>
            </div>
          </div>
        </button>

        {/* Portrait mix cards */}
        {daily.map((p) => (
          <MixCard key={p.id} playlist={p} />
        ))}
      </div>

      {rest.length > 0 && (
        <div className="mt-5">
          <p className="fe-label mb-2.5">{copy.forYou.mixes}</p>
          <ul className="flex flex-wrap gap-2.5">
            {rest.map((p) => (
              <li key={p.id}>
                <button
                  type="button"
                  className="fe-press flex items-center gap-2.5 rounded-full py-1 pl-1 pr-3.5 ring-1 ring-white/[0.08] hover:bg-white/[0.05] hover:ring-white/[0.14]"
                  title={copy.proto.hint}
                >
                  <Mosaic covers={p.covers} className="h-7 w-7 rounded-full!" />
                  <span className="text-[0.8125rem] font-medium text-[var(--fe-ink)]">
                    {p.name}
                  </span>
                  <span className="fe-ink-3 fe-mono text-[0.75rem]">
                    {integer(p.trackCount, lang)}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}

function MixCard({ playlist }: { playlist: FePlaylist }) {
  const { copy, lang } = useFe();
  const label = playlist.smartKind
    ? copy.forYou.labels[playlist.smartKind]
    : "";
  return (
    <button
      type="button"
      className="fe-tile-hover col-span-6 row-span-2 text-left lg:col-span-3"
      title={copy.proto.hint}
    >
      <div className="flex h-full flex-col rounded-[1.125rem] bg-[var(--fe-surface)] p-3">
        <Mosaic
          covers={playlist.covers}
          className="aspect-square w-full rounded-[0.75rem]!"
        />
        <div className="mt-auto px-1 pb-1 pt-4">
          <p className="fe-label">{label}</p>
          <p className="fe-title mt-1 truncate text-[1.0625rem]">
            {playlist.name}
          </p>
          <p className="fe-ink-3 fe-mono mt-0.5 text-[0.8125rem]">
            {fill(copy.sidebar.trackCount, {
              count: integer(playlist.trackCount, lang),
            })}
          </p>
        </div>
      </div>
    </button>
  );
}

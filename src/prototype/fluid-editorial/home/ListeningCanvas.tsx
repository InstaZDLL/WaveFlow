import { Heart, Pause, Play } from "lucide-react";
import { fixtures, trackFromAlbum, albumById } from "../fixtures";
import { fill } from "../copy";
import { duration, hours, integer, longDate } from "../format";
import { useFe } from "../useFe";
import { Cover } from "../ui/Cover";

/** "dimanche 18 octobre" → "Dimanche 18 octobre": only the first letter. */
function sentence(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

function greetingKey(): "morning" | "afternoon" | "evening" | "night" {
  const h = new Date().getHours();
  if (h >= 5 && h < 12) return "morning";
  if (h >= 12 && h < 18) return "afternoon";
  if (h >= 18 && h < 23) return "evening";
  return "night";
}

/**
 * The Listening Canvas replaces the greeting banner and the four stat
 * cards. Composition: a 7/5 split, text column left, artwork right, no
 * card around any of it — the atmosphere behind the page *is* the
 * container. The four counters survive as one line of tertiary ink
 * under the greeting; the last five covers hang off the artwork's
 * corner and switch the whole page's colour when clicked.
 */
export function ListeningCanvas() {
  const {
    copy,
    lang,
    current,
    isPlaying,
    togglePlay,
    positionMs,
    queue,
    play,
    liked,
    toggleLike,
    radioMood,
  } = useFe();
  const stats = fixtures.stats;
  const album =
    current.albumId != null ? albumById.get(current.albumId) : undefined;
  const remaining = Math.max(0, current.durationMs - positionMs);
  const strip = fixtures.lastPlayedAlbums.slice(0, 5);

  return (
    <section className="fe-canvas pt-1" aria-labelledby="fe-canvas-title">
      {/* The cover's own light, spread behind the title and the queue:
          the artwork, blurred wide and masked, so the record lights the
          whole canvas rather than sitting in its own rectangle. Keyed on
          the cover for a cross-fade on track change. */}
      <div
        key={current.cover ?? "none"}
        className="fe-canvas__aura"
        aria-hidden="true"
      >
        {current.cover && <img src={current.cover} alt="" decoding="async" />}
      </div>
      <div className="fe-canvas__text min-w-0">
        <p className="flex flex-wrap items-baseline text-[1.75rem] leading-none">
          <span className="fe-editorial text-[2rem]">
            {copy.greeting[greetingKey()]}
          </span>
          <span className="fe-ink-2 text-[1.125rem] font-[450]">, Probu</span>
        </p>
        <p className="fe-ink-3 fe-mono mt-3 flex flex-wrap gap-x-2 text-[0.8125rem] tracking-[0.01em]">
          <span>{sentence(longDate(lang))}</span>
          {[
            `${integer(stats.tracks, lang)} ${copy.stats.tracks}`,
            `${integer(stats.albums, lang)} ${copy.stats.albums}`,
            `${integer(stats.liked, lang)} ${copy.stats.liked}`,
            `${hours(stats.listenedMs, lang)} ${copy.stats.listened}`,
          ].map((item) => (
            <span key={item} className="whitespace-nowrap">
              <span className="mr-2 opacity-50">·</span>
              {item}
            </span>
          ))}
        </p>

        <div className="fe-canvas__resume">
          <p className="fe-label">
            {radioMood
              ? `${copy.canvas.radio} · ${copy.mood.moods[radioMood].title}`
              : copy.canvas.resumeKicker}
          </p>
          <h1
            id="fe-canvas-title"
            className="fe-display mt-3 max-w-[20ch] text-[clamp(2rem,2.6vw,3.25rem)]"
          >
            {current.title}
          </h1>
          <p className="fe-ink-2 mt-3 text-[1.0625rem] leading-snug">
            <span className="text-[var(--fe-ink)]">{current.artist}</span>
            {current.album && current.album !== current.title && (
              <>
                <span className="mx-2 opacity-50">·</span>
                {current.album}
              </>
            )}
            {album?.year && (
              <>
                <span className="mx-2 opacity-50">·</span>
                <span className="fe-mono">{album.year}</span>
              </>
            )}
          </p>

          <div className="mt-7 flex flex-wrap items-center gap-x-6 gap-y-3">
            <button type="button" className="fe-cta" onClick={togglePlay}>
              <span className="fe-cta__icon">
                {isPlaying ? (
                  <Pause size={14} className="fill-current" strokeWidth={0} />
                ) : (
                  <Play
                    size={14}
                    className="translate-x-px fill-current"
                    strokeWidth={0}
                  />
                )}
              </span>
              <span>{isPlaying ? copy.canvas.pause : copy.canvas.resume}</span>
              <span className="fe-mono text-[0.8125rem] font-medium opacity-70">
                {fill(copy.canvas.remaining, { time: duration(remaining) })}
              </span>
            </button>
            {album && (
              <button
                type="button"
                className="fe-link text-[0.875rem]"
                onClick={() => play(trackFromAlbum(album))}
              >
                {copy.canvas.playAlbum}
              </button>
            )}
            <button
              type="button"
              onClick={toggleLike}
              className="fe-icon-btn"
              aria-label={copy.player.like}
              aria-pressed={liked}
            >
              <Heart
                size={18}
                strokeWidth={1.6}
                className={liked ? "fill-current" : ""}
              />
            </button>
          </div>
        </div>
      </div>

      {/* Up next: three rows, dense, the only list in the canvas. Below
          the text on narrow windows; from xl it takes the middle column,
          its last row on the baseline of the actions. */}
      <div className="fe-canvas__queue max-w-[30rem]">
        <div>
          <p className="fe-label mb-2">{copy.canvas.upNext}</p>
          <ol className="divide-y divide-white/[0.06]">
            {queue.slice(0, 3).map((t) => (
              <li key={t.trackId}>
                <button
                  type="button"
                  onClick={() => play(t)}
                  className="group flex w-full items-center gap-3 py-1.5 text-left"
                >
                  <Cover
                    src={t.cover}
                    palette={t.palette}
                    alt={t.album ?? t.title}
                    decorative
                    className="h-9 w-9 rounded-md!"
                  />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-[0.8125rem] font-medium text-[var(--fe-ink)] group-hover:text-[var(--fe-accent)]">
                      {t.title}
                    </span>
                    <span className="fe-ink-3 block truncate text-[0.75rem]">
                      {t.artist}
                    </span>
                  </span>
                  <span className="fe-ink-3 fe-mono text-[0.75rem]">
                    {duration(t.durationMs)}
                  </span>
                </button>
              </li>
            ))}
          </ol>
        </div>
      </div>

      {/* Artwork: the largest thing on the page, sitting on its own glow,
          with the last five records hanging off its corner. */}
      <div className="fe-canvas__art relative">
        <figure className="relative w-[clamp(15rem,27vw,27rem)]">
          <Cover
            src={current.cover}
            palette={current.palette}
            alt={current.album ?? current.title}
            decorative
            className="fe-hero-cover aspect-square w-full rounded-[1.25rem]!"
          />
          <figcaption className="sr-only">
            {current.album ?? current.title}
          </figcaption>
          <ul
            className="absolute -bottom-5 -left-10 flex items-end gap-2 lg:-left-14"
            aria-label={copy.canvas.recentStrip}
          >
            {strip.map((a, i) => {
              const active = a.id === current.albumId;
              return (
                <li
                  key={a.id}
                  style={{
                    marginBottom: `${(strip.length - 1 - i) * 0.35}rem`,
                  }}
                >
                  <button
                    type="button"
                    onClick={() => play(trackFromAlbum(a))}
                    className={`fe-tile-hover fe-press block ${active ? "opacity-100" : "opacity-85 hover:opacity-100"}`}
                    aria-label={`${a.title} — ${a.artist}`}
                    aria-current={active ? "true" : undefined}
                  >
                    <Cover
                      src={a.cover}
                      palette={a.palette}
                      alt={a.title}
                      decorative
                      className={`h-14 w-14 rounded-[0.5rem]! ring-1 ring-black/40 ${active ? "ring-2 ring-[var(--fe-accent)]!" : ""}`}
                    />
                  </button>
                </li>
              );
            })}
          </ul>
        </figure>
      </div>
    </section>
  );
}

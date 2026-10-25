import { Pause, Play } from "lucide-react";
import { useTranslation } from "react-i18next";
import { usePlayerPosition } from "../../../../hooks/usePlayerPosition";
import { Artwork } from "../../../common/Artwork";
import { ArtistLink } from "../../../common/ArtistLink";
import { PrecomputedBlur } from "../../../common/PrecomputedBlur";
import type { Track } from "../../../../lib/tauri/track";
import type { QueueTrackPayload } from "../../../../lib/tauri/player";
import { blurSourceUrl, clock, longDate } from "./format";

function greetingKey(): "morning" | "afternoon" | "evening" | "night" {
  const h = new Date().getHours();
  if (h >= 5 && h < 12) return "morning";
  if (h >= 12 && h < 18) return "afternoon";
  if (h >= 18 && h < 23) return "evening";
  return "night";
}

/**
 * The time left on the playing track. Its own component so the 4 Hz
 * position tick re-renders this label alone, not the whole Home — the
 * reason the position lives outside the player context in the first
 * place (invariants § events).
 */
function RemainingTime({ durationMs }: { durationMs: number }) {
  const { t } = useTranslation();
  const positionMs = usePlayerPosition();
  return (
    <span className="fe-mono text-[0.8125rem] font-medium opacity-70">
      {t("home.fluid.remaining", {
        time: clock(Math.max(0, durationMs - positionMs)),
      })}
    </span>
  );
}

export interface StripAlbum {
  albumId: number;
  title: string;
  artist: string | null;
  artwork_path: string | null;
  artwork_path_1x: string | null;
  artwork_path_2x: string | null;
  /** Index of the play to start when the tile is clicked. */
  playIndex: number;
}

interface ListeningCanvasProps {
  profileName: string;
  /** One line of compact counters under the greeting. */
  statsLine: string[];
  /** The track the canvas is about: the playing one, or the last played. */
  track: Track | null;
  /** Whether `track` is the player's current track (vs. the last played). */
  isCurrent: boolean;
  isPlaying: boolean;
  /** The current track's length, for the time left on the CTA. */
  durationMs: number;
  radioLabel: string | null;
  onResume: () => void;
  onOpenAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  upNext: QueueTrackPayload[];
  onPlayQueueItem: (index: number) => void;
  strip: StripAlbum[];
  onPlayStrip: (playIndex: number) => void;
  /**
   * A masthead sits above the canvas (`fluid.greeting: "masthead"`): the
   * date is in its strip, so the counters line drops it, and the cover
   * takes a figure caption under it.
   */
  masthead?: boolean;
}

/**
 * The Listening Canvas replaces the greeting banner and the four stat
 * cards: a 7/5 split (three columns from 1700 px), text left, artwork
 * right, no card around any of it — the atmosphere behind the page is
 * the container. The four counters survive as one line of tertiary
 * ink under the greeting; the last five records hang off the artwork's
 * corner and start playing when clicked.
 */
export function ListeningCanvas({
  profileName,
  statsLine,
  track,
  isCurrent,
  isPlaying,
  durationMs,
  radioLabel,
  onResume,
  onOpenAlbum,
  onNavigateToArtist,
  upNext,
  onPlayQueueItem,
  strip,
  onPlayStrip,
  masthead = false,
}: ListeningCanvasProps) {
  const { t, i18n } = useTranslation();
  const blurSrc = track ? blurSourceUrl(track) : null;
  const showPause = isCurrent && isPlaying;
  const caption = !masthead
    ? null
    : track?.album_title && track?.artist_name
      ? t("editorial.lead.caption", {
          album: track.album_title,
          artist: track.artist_name,
        })
      : track?.artist_name
        ? t("editorial.lead.captionArtistOnly", { artist: track.artist_name })
        : t("editorial.lead.captionFallback");

  return (
    <section className="fe-canvas pt-1" aria-labelledby="fe-canvas-title">
      {/* The cover's own light, spread behind the title and the queue.
          Keyed on the URL for a cross-fade on track change. Darkened
          before the blur so a white sleeve cannot pale the ground under
          the counters line; blurred once, not once per frame. */}
      <div
        key={blurSrc ?? "none"}
        className="fe-canvas__aura"
        aria-hidden="true"
      >
        {blurSrc && (
          <PrecomputedBlur
            src={blurSrc}
            blur={96}
            brightness={0.7}
            saturate={1.05}
            scale={1.12}
          />
        )}
      </div>

      <div className="fe-canvas__text min-w-0">
        <p className="flex flex-wrap items-baseline text-[1.75rem] leading-none">
          <span className="fe-editorial fe-ink text-[2rem]">
            {t(`home.greeting.${greetingKey()}`)}
          </span>
          {profileName && (
            <span className="fe-ink-2 text-[1.125rem] font-[450]">
              , {profileName}
            </span>
          )}
        </p>
        <p className="fe-ink-3 fe-mono mt-3 flex flex-wrap gap-x-2 text-[0.8125rem] tracking-[0.01em]">
          {!masthead && <span>{longDate(i18n.language)}</span>}
          {statsLine.map((item, i) => (
            <span key={item} className="whitespace-nowrap">
              {(i > 0 || !masthead) && (
                <span className="mr-2 opacity-50">·</span>
              )}
              {item}
            </span>
          ))}
        </p>

        <div className="fe-canvas__resume">
          <p className="fe-label">
            {radioLabel ?? t("home.fluid.resumeKicker")}
          </p>
          <h1
            id="fe-canvas-title"
            className="fe-display fe-ink mt-3 max-w-[20ch] text-[clamp(2rem,2.6vw,3.25rem)]"
          >
            {track?.title ?? t("player.noTrack")}
          </h1>
          {track && (
            <p className="fe-ink-2 mt-3 text-[1.0625rem] leading-snug">
              <span className="fe-ink">
                <ArtistLink
                  name={track.artist_name}
                  artistIds={track.artist_ids}
                  onNavigate={onNavigateToArtist}
                  fallback="—"
                />
              </span>
              {track.album_title && track.album_title !== track.title && (
                <>
                  <span className="mx-2 opacity-50">·</span>
                  {track.album_title}
                </>
              )}
              {track.year && (
                <>
                  <span className="mx-2 opacity-50">·</span>
                  <span className="fe-mono">{track.year}</span>
                </>
              )}
            </p>
          )}

          <div className="mt-7 flex flex-wrap items-center gap-x-6 gap-y-3">
            <button
              type="button"
              className="fe-cta"
              onClick={onResume}
              disabled={!track}
            >
              <span className="fe-cta__icon">
                {showPause ? (
                  <Pause size={14} className="fill-current" strokeWidth={0} />
                ) : (
                  <Play
                    size={14}
                    className="translate-x-px fill-current"
                    strokeWidth={0}
                  />
                )}
              </span>
              <span>
                {showPause
                  ? t("home.fluid.pause")
                  : isCurrent
                    ? t("home.fluid.resume")
                    : t("home.fluid.play")}
              </span>
              {isCurrent && <RemainingTime durationMs={durationMs} />}
            </button>
            {track?.album_id != null && (
              <button
                type="button"
                className="fe-link text-[0.875rem]"
                onClick={() => onOpenAlbum(track.album_id!)}
              >
                {t("home.fluid.openAlbum")}
              </button>
            )}
          </div>
        </div>
      </div>

      {/* Up next: three rows, dense, the only list in the canvas. Below
          the text on narrow windows; from 1700 px it takes the middle
          column, its last row on the baseline of the actions. */}
      {upNext.length > 0 && (
        <div className="fe-canvas__queue max-w-[30rem]">
          <p className="fe-label mb-2">{t("home.fluid.upNext")}</p>
          <ol className="divide-y divide-[var(--fe-tone-2)]">
            {upNext.slice(0, 3).map((q, i) => (
              <li key={`${q.id}-${i}`}>
                <button
                  type="button"
                  onClick={() => onPlayQueueItem(i)}
                  className="group flex w-full items-center gap-3 py-1.5 text-left"
                >
                  <Artwork
                    path={q.artwork_path}
                    path1x={q.artwork_path_1x}
                    path2x={q.artwork_path_2x}
                    size="1x"
                    alt=""
                    className="fe-tile h-9 w-9"
                    iconSize={14}
                    rounded="md"
                  />
                  <span className="min-w-0 flex-1">
                    <span className="fe-ink block truncate text-[0.8125rem] font-medium group-hover:text-[var(--fe-accent)]">
                      {q.title}
                    </span>
                    <span className="fe-ink-3 block truncate text-[0.75rem]">
                      {q.artist_name ?? q.album_title ?? ""}
                    </span>
                  </span>
                  <span className="fe-ink-3 fe-mono text-[0.75rem]">
                    {clock(q.duration_ms)}
                  </span>
                </button>
              </li>
            ))}
          </ol>
        </div>
      )}

      {/* Artwork: the largest thing on the page, sitting on its own glow,
          with the last five records hanging off its corner. */}
      <div className="fe-canvas__art relative">
        <figure className="relative w-[clamp(15rem,27vw,27rem)]">
          <Artwork
            path={track?.artwork_path ?? null}
            path1x={track?.artwork_path_1x ?? null}
            path2x={track?.artwork_path_2x ?? null}
            size="full"
            alt={track?.album_title ?? track?.title ?? ""}
            className="fe-tile fe-hero-cover aspect-square w-full"
            iconSize={48}
            rounded="2xl"
          />
          {strip.length > 0 && (
            <ul
              className="absolute -bottom-5 -left-10 flex items-end gap-2 lg:-left-14"
              aria-label={t("home.fluid.recentStrip")}
            >
              {strip.map((a, i) => {
                const active = track?.album_id === a.albumId;
                return (
                  <li
                    key={a.albumId}
                    style={{
                      marginBottom: `${(strip.length - 1 - i) * 0.35}rem`,
                    }}
                  >
                    <button
                      type="button"
                      onClick={() => onPlayStrip(a.playIndex)}
                      className={`fe-tile-hover fe-press block ${active ? "opacity-100" : "opacity-85 hover:opacity-100"}`}
                      aria-label={
                        a.artist ? `${a.title} — ${a.artist}` : a.title
                      }
                      aria-current={active ? "true" : undefined}
                    >
                      <Artwork
                        path={a.artwork_path}
                        path1x={a.artwork_path_1x}
                        path2x={a.artwork_path_2x}
                        size="2x"
                        alt=""
                        className={`fe-tile h-14 w-14 ring-1 ring-black/40 ${active ? "ring-2 ring-[var(--fe-accent)]!" : ""}`}
                        iconSize={18}
                        rounded="lg"
                      />
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </figure>
        {/* The "Fig." caption, under the strip rather than inside the
            figure: the strip is anchored to the figure's foot and would
            follow a caption placed there. */}
        {caption && <p className="fe-canvas__caption">{caption}</p>}
      </div>
    </section>
  );
}

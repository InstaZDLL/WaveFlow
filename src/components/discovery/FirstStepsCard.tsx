import { useTranslation } from "react-i18next";
import { Check, X } from "lucide-react";
import { useSkin } from "../../hooks/useSkin";
import { usePlayer } from "../../hooks/usePlayer";
import {
  DISCOVERIES,
  openSmartPlaylistEditor,
  openView,
  type DiscoveryId,
} from "../../lib/discovery";
import { openDesktopLyrics } from "../../lib/tauri/desktopLyrics";
import { openTagWorkshop } from "../../lib/tagWorkshop";
import { listAlbums } from "../../lib/tauri/browse";
import { FeatureArt, type FeatureArtKind } from "./FeatureArt";
import { shuffledLibrary } from "./shuffledLibrary";
import { useFirstSteps } from "./useFirstSteps";
import "./discovery.css";

const ART: Record<DiscoveryId, FeatureArtKind> = {
  immersive: "immersive",
  miniPlayer: "miniPlayer",
  desktopLyrics: "desktopLyrics",
  smartPlaylist: "smartPlaylist",
  enrichment: "enrichment",
  tagWorkshop: "tagWorkshop",
  wrapped: "wrapped",
  plugin: "plugins",
};

/**
 * The Home's "First steps": features a new listener would not find by
 * browsing, each a click away and ticked by its first use, wherever it
 * happens (`noteDiscovery`). Shown in both layouts until it is closed or
 * every item is ticked. The scenes hold still until an item is hovered,
 * so the Home stays calm.
 */
export function FirstStepsCard() {
  const { t } = useTranslation();
  const { skin } = useSkin();
  const fluid = skin.layout === "fluid";
  const { ready, done, hidden, hide } = useFirstSteps();
  const { currentTrack, playTracks, openImmersive } = usePlayer();

  if (!ready || hidden || done.size === DISCOVERIES.length) return null;

  const run = async (id: DiscoveryId) => {
    try {
      switch (id) {
        case "immersive":
          // The view shows what plays: with nothing playing, start the
          // library first.
          if (!currentTrack) {
            const tracks = await shuffledLibrary();
            if (tracks.length === 0) {
              openView({ view: "library" });
              return;
            }
            await playTracks(tracks, 0, { type: "library", id: null });
          }
          openImmersive("nowPlaying");
          return;
        case "miniPlayer": {
          const { openMiniPlayer } = await import("../../lib/miniPlayer");
          await openMiniPlayer();
          return;
        }
        case "desktopLyrics":
          await openDesktopLyrics();
          return;
        case "smartPlaylist":
          openSmartPlaylistEditor();
          return;
        case "enrichment":
          openView({ view: "settings", settingsCategory: "library" });
          return;
        case "tagWorkshop": {
          // The album playing, else the one added last.
          let albumId = currentTrack?.album_id ?? null;
          if (albumId == null) {
            const albums = await listAlbums(null, {
              orderBy: "added_at",
              direction: "desc",
            });
            albumId = albums[0]?.id ?? null;
          }
          if (albumId == null) openView({ view: "library" });
          else
            openTagWorkshop({ source: { kind: "album", album_id: albumId } });
          return;
        }
        case "wrapped":
          openView({ view: "wrapped" });
          return;
        case "plugin":
          openView({ view: "extensions" });
          return;
      }
    } catch (err) {
      console.error(`[FirstSteps] ${id} failed`, err);
    }
  };

  return (
    <section
      aria-labelledby="first-steps-title"
      className={`first-steps @container relative ${
        fluid
          ? "rounded-[1.125rem] bg-[var(--fe-surface)] px-6 py-5"
          : "rounded-xl border border-zinc-200 bg-white p-5 dark:border-zinc-800 dark:bg-zinc-900"
      }`}
    >
      <div className="flex items-start gap-4">
        <div className="min-w-0 flex-1">
          <span
            className={
              fluid
                ? "fe-label block"
                : "block text-[11px] font-semibold uppercase tracking-widest text-emerald-600 dark:text-emerald-400"
            }
          >
            {t("firstSteps.kicker")}
          </span>
          <h2
            id="first-steps-title"
            className={
              fluid
                ? "fe-title fe-ink mt-1 text-[1.375rem]"
                : "mt-0.5 text-base font-semibold text-zinc-900 dark:text-white"
            }
          >
            {t("firstSteps.title")}
          </h2>
          <p
            className={
              fluid
                ? "fe-ink-3 mt-1 text-[0.8125rem]"
                : "mt-0.5 text-xs tabular-nums text-zinc-500 dark:text-zinc-400"
            }
          >
            {t("firstSteps.progress", {
              done: done.size,
              total: DISCOVERIES.length,
            })}
          </p>
        </div>
        <button
          type="button"
          onClick={hide}
          aria-label={t("firstSteps.hide")}
          title={t("firstSteps.hide")}
          className={
            fluid
              ? "fe-icon-btn shrink-0"
              : "shrink-0 rounded-md p-1.5 text-zinc-400 transition-colors hover:bg-zinc-100 hover:text-zinc-700 dark:hover:bg-zinc-800 dark:hover:text-zinc-200"
          }
        >
          <X size={16} />
        </button>
      </div>

      <div
        className="mt-4 h-1 overflow-hidden rounded-full bg-zinc-200 dark:bg-white/10"
        aria-hidden="true"
      >
        <div
          className="onboarding-scan__bar h-full w-full rounded-full bg-emerald-500"
          style={{ transform: `scaleX(${done.size / DISCOVERIES.length})` }}
        />
      </div>

      <ul className="mt-5 grid grid-cols-1 gap-2 @xl:grid-cols-2 @5xl:grid-cols-4">
        {DISCOVERIES.map((id) => {
          const ticked = done.has(id);
          return (
            <li key={id}>
              <button
                type="button"
                onClick={() => void run(id)}
                data-done={ticked || undefined}
                className={`first-steps__item group flex w-full items-center gap-3 rounded-2xl p-2.5 text-start transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${
                  fluid
                    ? "hover:bg-[var(--fe-tone-2)]"
                    : "hover:bg-zinc-50 dark:hover:bg-white/5"
                }`}
              >
                <span
                  className={`first-steps__art relative h-12 w-16 shrink-0 overflow-hidden rounded-xl ${
                    fluid
                      ? "bg-[var(--fe-tone-1)] text-[var(--fe-ink-2)]"
                      : "bg-zinc-100 text-zinc-500 dark:bg-white/5 dark:text-zinc-400"
                  }`}
                >
                  <FeatureArt kind={ART[id]} />
                  {ticked && (
                    <span className="absolute inset-0 flex items-center justify-center bg-emerald-500/85 text-white">
                      <Check size={18} strokeWidth={3} aria-hidden="true" />
                    </span>
                  )}
                </span>
                <span className="min-w-0">
                  <span
                    className={`block truncate text-sm font-semibold ${
                      fluid ? "fe-ink" : "text-zinc-900 dark:text-white"
                    } ${ticked ? "line-through decoration-1 opacity-60" : ""}`}
                  >
                    {t(`firstSteps.items.${id}.title`)}
                    {ticked && (
                      <span className="sr-only"> {t("firstSteps.done")}</span>
                    )}
                  </span>
                  <span
                    className={`block text-xs leading-snug ${
                      fluid ? "fe-ink-3" : "text-zinc-500 dark:text-zinc-400"
                    }`}
                  >
                    {t(`firstSteps.items.${id}.hint`)}
                  </span>
                </span>
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

import { useState } from "react";
import { useTranslation } from "react-i18next";
import {
  X,
  Heart,
  Star,
  Share2,
  Download,
  Copy,
  Loader2,
  Check,
  Mic2,
} from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { PlaybackControls } from "./PlaybackControls";
import { ProgressBar } from "./ProgressBar";
import { VolumeControl } from "./VolumeControl";
import { usePlayer } from "../../hooks/usePlayer";
import { useWebRadioFavorites } from "../../hooks/useWebRadioFavorites";
import { isRadioTrack } from "../../lib/playerSources";
import { SpectrumVisualizer } from "./SpectrumVisualizer";
import { pickSaveFile } from "../../lib/tauri/dialog";
import { saveShareImage } from "../../lib/tauri/share";
import { renderNowPlayingCard } from "../../lib/nowPlayingCard";

interface FullscreenNowPlayingProps {
  onClose: () => void;
  onOpenLyrics: () => void;
  onNavigateToArtist: (artistId: number) => void;
  isLiked: boolean;
  onToggleLike: () => void;
}

/**
 * Apple-Music-style immersive Now Playing overlay. Reuses the same
 * playback controls + progress bar as the bottom player so behaviour
 * stays identical (drag-to-seek, repeat / shuffle states, etc.). The
 * cover is the visual anchor — large + centred — over a blurred copy
 * of itself for the background.
 *
 * Closes on Escape or via the X button. Does **not** persist any
 * state — opening + closing is purely a UI concern owned by the
 * parent (PlayerBar).
 */
export function FullscreenNowPlaying({
  onClose,
  onOpenLyrics,
  onNavigateToArtist,
  isLiked,
  onToggleLike,
}: FullscreenNowPlayingProps) {
  const { t } = useTranslation();
  const { currentTrack, currentRadioStation } = usePlayer();
  // Live radio: favorite the STATION (★) instead of liking a track
  // (♥) — a radio session has a negative sentinel id with no library
  // row to like. Mirrors the PlayerBar / mini-player treatment.
  const radioFavorites = useWebRadioFavorites();
  const stationFavorited =
    currentRadioStation != null &&
    radioFavorites.isFavorite(currentRadioStation.id);
  // Escape close + focus trap. The overlay is only mounted while
  // open, so passing `true` is correct — no isOpen prop needed.
  const dialogRef = useModalA11y<HTMLDivElement>(true, onClose);

  const title = currentTrack?.title ?? t("player.noTrack");
  const album = currentTrack?.album_title;

  // Share menu state — open/close is purely UI, no other view needs it.
  const [shareOpen, setShareOpen] = useState(false);
  const [sharing, setSharing] = useState<
    "idle" | "saving" | "copying" | "done"
  >("idle");

  // Sanitise the track title for use as a default filename — the
  // native save dialog will reject reserved characters on Windows
  // (`< > : " / \ | ? *`), and a stripped fallback is friendlier than
  // an opaque "card.png".
  const sanitizeFilename = (s: string): string =>
    s
      // eslint-disable-next-line no-control-regex
      .replace(/[<>:"/\\|?*\x00-\x1f]/g, "_")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 80) || "track";

  const buildCard = async (): Promise<Blob> => {
    if (!currentTrack) throw new Error("no current track");
    return renderNowPlayingCard(currentTrack, {
      labels: {
        nowPlaying: t("nowPlaying.share.eyebrow"),
        on: t("nowPlaying.share.on"),
      },
    });
  };

  const handleSave = async () => {
    if (!currentTrack) return;
    try {
      setSharing("saving");
      const defaultName = `${sanitizeFilename(currentTrack.title)} - WaveFlow.png`;
      const target = await pickSaveFile(defaultName, ["png"]);
      if (!target) {
        setSharing("idle");
        return;
      }
      const blob = await buildCard();
      const bytes = new Uint8Array(await blob.arrayBuffer());
      await saveShareImage(bytes, target);
      setSharing("done");
      window.setTimeout(() => setSharing("idle"), 2000);
    } catch (err) {
      console.error("[FullscreenNowPlaying] save image failed", err);
      setSharing("idle");
    }
  };

  const handleCopy = async () => {
    if (!currentTrack) return;
    try {
      setSharing("copying");
      const blob = await buildCard();
      await navigator.clipboard.write([
        new ClipboardItem({ "image/png": blob }),
      ]);
      setSharing("done");
      window.setTimeout(() => setSharing("idle"), 2000);
    } catch (err) {
      console.error("[FullscreenNowPlaying] copy image failed", err);
      setSharing("idle");
    }
  };

  return (
    <div
      ref={dialogRef}
      role="dialog"
      aria-modal="true"
      aria-label={t("playerBar.openFullscreen")}
      className="fixed inset-0 z-100 bg-zinc-950"
    >
      {/* Blurred artwork background — falls back to a flat dark
          gradient when the track has no cover. Same recipe as the
          fullscreen lyrics overlay so they feel like siblings.
          The `animate-fade-in` lives here (not on the outer wrapper)
          so the opaque `bg-zinc-950` above paints solid from frame 1
          — without that base the wrapper's own opacity tween would
          let the home view bleed through during the 300 ms ramp. */}
      <div className="absolute inset-0 overflow-hidden animate-fade-in">
        {currentTrack?.artwork_path ? (
          <Artwork
            path={currentTrack.artwork_path}
            path1x={currentTrack.artwork_path_1x}
            path2x={currentTrack.artwork_path_2x}
            size="full"
            className="w-full h-full scale-150 blur-3xl"
            alt=""
            rounded="md"
          />
        ) : (
          <div className="w-full h-full bg-linear-to-br from-zinc-800 to-zinc-950" />
        )}
        <div className="absolute inset-0 bg-black/65" />
      </div>

      {/* Foreground */}
      <div className="relative h-full flex flex-col text-white animate-fade-in">
        {/* Top bar — lyrics switcher + share + close. Controls live at
            the bottom so the cover gets the visual centre. */}
        <div className="flex items-center justify-end gap-3 px-8 py-6 shrink-0">
          {currentTrack && (
            <button
              type="button"
              onClick={onOpenLyrics}
              aria-label={t("playerBar.lyrics")}
              title={t("playerBar.lyrics")}
              className="p-2.5 rounded-full bg-white/10 hover:bg-white/20 transition-colors"
            >
              <Mic2 size={22} />
            </button>
          )}
          {currentTrack && (
            <div className="relative">
              <button
                type="button"
                onClick={() => setShareOpen((s) => !s)}
                disabled={sharing === "saving" || sharing === "copying"}
                aria-label={t("nowPlaying.share.open")}
                className="p-2.5 rounded-full bg-white/10 hover:bg-white/20 transition-colors disabled:opacity-50"
              >
                {sharing === "saving" || sharing === "copying" ? (
                  <Loader2 size={22} className="animate-spin" />
                ) : sharing === "done" ? (
                  <Check size={22} />
                ) : (
                  <Share2 size={22} />
                )}
              </button>
              {shareOpen && (
                <div className="absolute right-0 top-full mt-2 min-w-56 rounded-2xl bg-zinc-900/95 backdrop-blur-md border border-white/10 shadow-2xl overflow-hidden z-10">
                  <button
                    onClick={async () => {
                      setShareOpen(false);
                      await handleSave();
                    }}
                    className="w-full px-4 py-3 flex items-center gap-3 hover:bg-white/10 transition-colors text-sm"
                  >
                    <Download size={16} className="opacity-70" />
                    {t("nowPlaying.share.save")}
                  </button>
                  <button
                    onClick={async () => {
                      setShareOpen(false);
                      await handleCopy();
                    }}
                    className="w-full px-4 py-3 flex items-center gap-3 hover:bg-white/10 transition-colors text-sm border-t border-white/5"
                  >
                    <Copy size={16} className="opacity-70" />
                    {t("nowPlaying.share.copy")}
                  </button>
                </div>
              )}
            </div>
          )}
          <button
            type="button"
            onClick={onClose}
            aria-label={t("common.close")}
            className="p-2.5 rounded-full bg-white/10 hover:bg-white/20 transition-colors"
          >
            <X size={22} />
          </button>
        </div>

        {/* Cover hero. The cover is sized so the full layout (top
            bar + cover + metadata + visualizer + transport) fits a
            1080p viewport at 125 % DPI without overflow. Previous cap
            of `min(60vh, 32rem)` produced a 512 px square that pushed
            the visualizer into the metadata on smaller screens
            (reported in #54). 45 vh / 26 rem keeps the hero
            visually dominant while leaving ~200 px for the controls
            stack underneath. */}
        <div className="flex-1 flex flex-col items-center justify-center px-8 min-h-0">
          <div className="w-full max-w-[min(45vh,26rem)] aspect-square mb-6">
            <Artwork
              path={currentTrack?.artwork_path ?? null}
              path1x={currentTrack?.artwork_path_1x ?? null}
              path2x={currentTrack?.artwork_path_2x ?? null}
              size="full"
              className="w-full h-full shadow-2xl"
              iconSize={96}
              alt={title}
              rounded="2xl"
            />
          </div>

          {/* Track info — title + clickable artist + album. */}
          <div className="text-center max-w-2xl w-full mb-2">
            <h1 className="text-3xl md:text-4xl font-bold truncate">{title}</h1>
            <div className="mt-2 text-lg text-white/80 flex items-center justify-center gap-3 flex-wrap">
              {currentTrack?.artist_name && (
                <ArtistLink
                  name={currentTrack.artist_name}
                  artistIds={currentTrack.artist_ids}
                  onNavigate={(id) => {
                    onNavigateToArtist(id);
                    onClose();
                  }}
                />
              )}
              {album && (
                <>
                  <span className="text-white/40">·</span>
                  <span className="truncate">{album}</span>
                </>
              )}
            </div>
          </div>
        </div>

        {/* Bottom transport — progress on top, controls below.
            Width-capped so on ultrawide monitors the layout stays
            visually balanced instead of stretching edge-to-edge. */}
        <div className="px-8 pb-10 shrink-0">
          <div className="max-w-3xl mx-auto fullscreen-now-playing-controls">
            {/* Spectrum visualizer sits above the progress bar.
                Renders an empty canvas + draws nothing when the
                backend visualizer toggle is off, so it's safe to
                always mount. `glow` mode = white bars suited to the
                dim immersive backdrop. */}
            <SpectrumVisualizer className="w-full h-16 mb-2 opacity-80" glow />
            <ProgressBar />
            <div className="flex items-center justify-between gap-6 mt-2">
              {/* Left cluster — like button. Lives down here (not in
                  the hero) so the visualizer canvas above never sits
                  underneath an interactive control. */}
              <div className="flex-1 min-w-0 flex justify-start">
                {currentRadioStation ? (
                  <button
                    type="button"
                    onClick={() =>
                      radioFavorites.toggleFavorite(currentRadioStation)
                    }
                    aria-label={
                      stationFavorited
                        ? t("webRadio.removeFavorite")
                        : t("webRadio.addFavorite")
                    }
                    aria-pressed={stationFavorited}
                    className={`p-2 rounded-full transition-colors ${
                      stationFavorited
                        ? "text-amber-400 hover:text-amber-300"
                        : "text-white/60 hover:text-amber-400"
                    }`}
                  >
                    <Star
                      size={20}
                      fill={stationFavorited ? "currentColor" : "none"}
                    />
                  </button>
                ) : currentTrack && !isRadioTrack(currentTrack) ? (
                  // `!isRadioTrack` guards the hydration race + idle tail:
                  // a radio sentinel track (negative id) must never show a
                  // ♥ like (no library row), even before
                  // `currentRadioStation` arrives.
                  <button
                    type="button"
                    onClick={onToggleLike}
                    aria-label={isLiked ? t("liked.unlike") : t("liked.like")}
                    className={`p-2 rounded-full transition-colors ${
                      isLiked
                        ? "text-pink-400 hover:text-pink-300"
                        : "text-white/60 hover:text-pink-400"
                    }`}
                  >
                    <Heart
                      size={20}
                      className={isLiked ? "fill-current" : ""}
                    />
                  </button>
                ) : null}
              </div>
              <PlaybackControls />
              <div className="flex-1 min-w-0 flex justify-end">
                <VolumeControl />
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Share2, Download, Copy, Loader2, Check } from "lucide-react";
import type { Track } from "../../lib/tauri/track";
import { pickSaveFile } from "../../lib/tauri/dialog";
import { saveShareImage } from "../../lib/tauri/share";
import { renderNowPlayingCard } from "../../lib/nowPlayingCard";

interface ImmersiveShareButtonProps {
  track: Track;
}

/**
 * Share-as-PNG control for the immersive view top bar. Renders a
 * Now-Playing card off the current track and either saves it via the
 * native dialog or copies it to the clipboard. Self-contained (owns its
 * open/sharing state) so the orchestrator's top bar stays declarative.
 *
 * Lifted verbatim from the old `FullscreenNowPlaying` share menu.
 */
export function ImmersiveShareButton({ track }: ImmersiveShareButtonProps) {
  const { t } = useTranslation();
  const [shareOpen, setShareOpen] = useState(false);
  const [sharing, setSharing] = useState<
    "idle" | "saving" | "copying" | "done"
  >("idle");

  // Sanitise the track title for use as a default filename — the native
  // save dialog rejects reserved characters on Windows (`< > : " / \ |
  // ? *`), and a stripped fallback is friendlier than an opaque
  // "card.png".
  const sanitizeFilename = (s: string): string =>
    s
      // eslint-disable-next-line no-control-regex
      .replace(/[<>:"/\\|?*\x00-\x1f]/g, "_")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 80) || "track";

  const buildCard = async (): Promise<Blob> =>
    renderNowPlayingCard(track, {
      labels: {
        nowPlaying: t("nowPlaying.share.eyebrow"),
        on: t("nowPlaying.share.on"),
      },
    });

  const handleSave = async () => {
    try {
      setSharing("saving");
      const defaultName = `${sanitizeFilename(track.title)} - WaveFlow.png`;
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
      console.error("[ImmersiveShareButton] save image failed", err);
      setSharing("idle");
    }
  };

  const handleCopy = async () => {
    try {
      setSharing("copying");
      const blob = await buildCard();
      await navigator.clipboard.write([
        new ClipboardItem({ "image/png": blob }),
      ]);
      setSharing("done");
      window.setTimeout(() => setSharing("idle"), 2000);
    } catch (err) {
      console.error("[ImmersiveShareButton] copy image failed", err);
      setSharing("idle");
    }
  };

  return (
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
            type="button"
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
            type="button"
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
  );
}

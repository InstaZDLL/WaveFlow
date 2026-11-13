import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { ArrowLeft, TriangleAlert, VolumeX } from "lucide-react";

import { useModalA11y } from "../../hooks/useModalA11y";
import { usePlayableVideo } from "../../hooks/usePlayableVideo";
import { usePlayer } from "../../hooks/usePlayer";
import { closeClip, useOpenClip, type OpenClip } from "../../lib/clipPlayer";
import { playerSetClipOpen, type ClipControl } from "../../lib/tauri/clip";

/** How far the arrow keys move the clip, in seconds. */
const ARROW_SEEK_S = 5;

/**
 * The music video on screen, if one is open (#1073): over the whole
 * immersive view, theater mode included, with the clip's own sound, its
 * own length and the webview's own controls. A clip is rarely the album
 * version — an intro, scenes, an outro — so nothing ties it to the track:
 * the track waits paused at its position while the clip plays.
 */
export function ClipStage() {
  const clip = useOpenClip();
  // Mounted for as long as the immersive view is: closing the view, by
  // whatever path, closes the clip with it, so it does not come back at
  // the next opening.
  useEffect(() => closeClip, []);
  if (!clip) return null;
  return <ClipPlayer key={`${clip.trackId}:${clip.localPath}`} clip={clip} />;
}

function ClipPlayer({ clip }: { clip: OpenClip }) {
  const { t } = useTranslation();
  const { isPlaying, togglePlayback, volume, outputMode } = usePlayer();
  const videoRef = useRef<HTMLVideoElement>(null);
  const video = usePlayableVideo(videoRef, clip.localPath, false);
  const [failed, setFailed] = useState(false);

  // Whether the track was playing when the clip opened: going back to it
  // by hand picks it up again. Read once, at the first render.
  const [resumeTrack] = useState(isPlaying);
  const isPlayingRef = useRef(isPlaying);
  const togglePlaybackRef = useRef(togglePlayback);
  useEffect(() => {
    isPlayingRef.current = isPlaying;
    togglePlaybackRef.current = togglePlayback;
  });

  // The track pauses for the clip, through the same path as the Play
  // button, so the OS media overlay and Discord show it paused.
  useEffect(() => {
    if (isPlayingRef.current) void togglePlaybackRef.current();
  }, []);

  // The track starting again — Next pressed, a track picked in the queue,
  // MPD — means the user went back to the music: the clip gives way. Only
  // a start after the clip has seen the track stopped counts, or the pause
  // just above would race its own state change.
  const sawStopped = useRef(!isPlaying);
  useEffect(() => {
    if (!isPlaying) {
      sawStopped.current = true;
    } else if (sawStopped.current) {
      closeClip();
    }
  }, [isPlaying]);

  // Media keys, the tray, the taskbar and the global shortcut reach the
  // clip while it is open (`player_actions::set_clip_open`).
  useEffect(() => {
    void playerSetClipOpen(true).catch((err) =>
      console.error("[ClipStage] set clip open failed", err),
    );
    const unlisten = listen<ClipControl>("clip:control", ({ payload }) => {
      const element = videoRef.current;
      if (!element) return;
      if (payload === "pause" || (payload === "toggle" && !element.paused)) {
        element.pause();
      } else {
        void element.play().catch(() => undefined);
      }
    });
    return () => {
      void unlisten.then((stop) => stop());
      void playerSetClipOpen(false).catch((err) =>
        console.error("[ClipStage] set clip closed failed", err),
      );
    };
  }, []);

  /** Back to the track by hand: it plays again if it was playing. */
  const backToAudio = useCallback(() => {
    closeClip();
    if (resumeTrack && !isPlayingRef.current) {
      void togglePlaybackRef.current();
    }
  }, [resumeTrack]);

  const dialogRef = useModalA11y<HTMLDivElement>(true, backToAudio);

  // The clip starts at the app's volume; its own control changes only the
  // clip. Autoplay with sound can be refused by the webview: the controls
  // are there to start it by hand.
  const onCanPlay = () => {
    const element = videoRef.current;
    if (!element || element.dataset.started) return;
    element.dataset.started = "1";
    element.volume = Math.min(1, Math.max(0, volume / 100));
    void element.play().catch(() => undefined);
  };

  // Space and the arrows act on the clip, not on the paused track. A
  // focused button keeps its own Space; the global shortcut must not hear
  // it either.
  const onKeyDownCapture = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const element = videoRef.current;
    const target = event.target as HTMLElement;
    if (!element || target.closest("button")) return;
    let handled = true;
    if (event.key === " " || event.key === "k") {
      if (element.paused) void element.play().catch(() => undefined);
      else element.pause();
    } else if (event.key === "ArrowLeft") {
      element.currentTime = Math.max(0, element.currentTime - ARROW_SEEK_S);
    } else if (event.key === "ArrowRight") {
      element.currentTime = element.currentTime + ARROW_SEEK_S;
    } else {
      handled = false;
    }
    if (handled) {
      event.preventDefault();
      event.stopPropagation();
    }
  };
  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key === " ") event.stopPropagation();
  };

  const exclusive = outputMode === "exclusive" || outputMode === "dop";
  const unplayable = failed || video.failed;

  return (
    <div
      ref={dialogRef}
      role="dialog"
      aria-modal="true"
      aria-label={t("clip.label", { title: clip.title })}
      className="absolute inset-0 z-20 flex items-center justify-center bg-black text-white"
      onKeyDownCapture={onKeyDownCapture}
      onKeyDown={onKeyDown}
      tabIndex={-1}
    >
      {!unplayable && (
        <video
          ref={videoRef}
          controls
          playsInline
          className="h-full w-full object-contain"
          onCanPlay={onCanPlay}
          // The end of the clip is the end of the visit: back to the
          // cover, the track still paused where it was.
          onEnded={closeClip}
          onError={() => setFailed(true)}
        />
      )}

      {unplayable && (
        <div
          role="alert"
          className="mx-6 flex max-w-md flex-col items-center gap-3 text-center"
        >
          <TriangleAlert size={32} className="text-amber-400" />
          <p className="text-lg font-semibold">{t("clip.unplayableTitle")}</p>
          <p className="text-sm text-white/70">{t("clip.unplayableBody")}</p>
        </div>
      )}

      <div
        data-theater-chrome
        className="absolute top-0 inset-x-0 flex items-start justify-between gap-4 px-8 py-6 pointer-events-none *:pointer-events-auto"
      >
        <button
          type="button"
          onClick={backToAudio}
          className="flex items-center gap-2 rounded-full bg-black/50 px-4 py-2.5 text-sm font-medium text-white backdrop-blur-sm transition-colors hover:bg-black/70"
        >
          <ArrowLeft className="rtl:rotate-180" size={18} />
          {t("clip.backToAudio")}
        </button>
        <div className="min-w-0 text-end">
          <p className="truncate text-sm font-semibold">{clip.title}</p>
          {clip.artist && (
            <p className="truncate text-xs text-white/70">{clip.artist}</p>
          )}
        </div>
      </div>

      {exclusive && !unplayable && (
        <div
          data-theater-chrome
          role="status"
          className="pointer-events-none absolute inset-x-0 bottom-20 mx-auto flex w-fit items-center gap-2 rounded-full bg-black/70 px-4 py-2 text-xs text-white/90"
        >
          <VolumeX size={14} />
          {t("clip.exclusiveNoSound")}
        </div>
      )}
    </div>
  );
}

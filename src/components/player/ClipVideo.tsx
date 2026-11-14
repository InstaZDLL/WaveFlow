import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { TriangleAlert } from "lucide-react";

import { usePlayableVideo } from "../../hooks/usePlayableVideo";
import { usePlayer } from "../../hooks/usePlayer";
import {
  attachClipVideo,
  clipState,
  detachClipVideo,
  markClipPositioned,
  playClipElement,
  reportClipProgress,
  requestBackToAudio,
  useClipState,
} from "../../lib/clipPlayer";

/** Stop the clip this far before its end when the track's position would
 *  put it past it, so switching late in a song still shows a frame. */
const END_MARGIN_MS = 1000;

/**
 * The music video's `<video>` (#1073), in whichever surface shows it — the
 * now-playing panel or the immersive view. Driven by WaveFlow's own
 * controls through [`clipPlayer`](../../lib/clipPlayer.ts): no browser
 * controls, no picture-in-picture, no browser menu, so what is offered does
 * not depend on the webview and offers nothing WaveFlow does not do
 * (downloading the file, a playback rate of its own).
 *
 * Mounting resumes the clip where the store says it is and in its play
 * state, so moving from one surface to the other carries both across.
 */
export function ClipVideo({ className }: { className?: string }) {
  const clip = useClipState().clip;
  if (!clip) return null;
  // One element per session: every opening (the same clip again, or a
  // new one after one that failed) gets an element of its own, and what
  // an element reports is about its session only.
  return (
    <ClipElement
      key={clip.session}
      session={clip.session}
      localPath={clip.localPath}
      className={className}
    />
  );
}

function ClipElement({
  session,
  localPath,
  className,
}: {
  session: number;
  localPath: string;
  className?: string;
}) {
  const { t } = useTranslation();
  const { volume } = usePlayer();
  const { failed: storeFailed } = useClipState();
  const ref = useRef<HTMLVideoElement>(null);
  // Until its metadata loads and it is put at the clip's position, the
  // element's time is 0 and says nothing about where the clip is.
  const positioned = useRef(false);

  // Declared before `usePlayableVideo`, so its cleanup runs first: on Linux
  // the hook's own cleanup reloads the element, which puts it back at 0.
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    attachClipVideo(session, element);
    return () => detachClipVideo(session, element);
  }, [session]);

  const playable = usePlayableVideo(ref, localPath, false);
  const failed = storeFailed || playable.failed;

  // The app's volume, followed while the clip plays: the player bar's
  // slider is the one the user reaches for.
  const appVolume = Math.min(1, Math.max(0, volume / 100));
  const appVolumeRef = useRef(appVolume);
  useEffect(() => {
    appVolumeRef.current = appVolume;
    if (ref.current) ref.current.volume = appVolume;
  }, [appVolume]);
  // Set again once the webview has opened its audio stream. WebKitGTK's
  // stream is restored by the sound server to the level it last had under
  // the app's name (PipeWire remembers it per application), and the
  // element takes that level over: a clip once left at 0 came back silent
  // at every later launch, the slider at 100 or not.
  const applyAppVolume = (element: HTMLVideoElement) => {
    if (element.volume !== appVolumeRef.current) {
      element.volume = appVolumeRef.current;
    }
  };

  const onLoadedMetadata = () => {
    const element = ref.current;
    if (!element) return;
    const durationMs = Number.isFinite(element.duration)
      ? Math.round(element.duration * 1000)
      : 0;
    const { positionMs, paused } = clipState();
    const target =
      durationMs > 0
        ? Math.min(positionMs, Math.max(0, durationMs - END_MARGIN_MS))
        : positionMs;
    element.currentTime = target / 1000;
    const aspect =
      element.videoWidth > 0 && element.videoHeight > 0
        ? element.videoWidth / element.videoHeight
        : null;
    positioned.current = true;
    markClipPositioned(session);
    reportClipProgress(session, { durationMs, positionMs: target, aspect });
    // Autoplay with sound can be refused by the webview: the store then
    // says paused, and the player bar's Play button starts it.
    if (!paused) playClipElement(session, element);
  };

  if (failed) {
    return (
      <div
        role="alert"
        className={`flex flex-col items-center justify-center gap-2 bg-black p-4 text-center text-white ${className ?? ""}`}
      >
        <TriangleAlert size={28} className="text-amber-400" />
        <p className="text-sm font-semibold">{t("clip.unplayableTitle")}</p>
        <p className="text-xs text-white/70">{t("clip.unplayableBody")}</p>
      </div>
    );
  }
  return (
    <video
      ref={ref}
      playsInline
      disablePictureInPicture
      // No browser menu over the clip: it offers saving the file and
      // controls of its own.
      onContextMenu={(event) => event.preventDefault()}
      className={`bg-black object-contain ${className ?? ""}`}
      onLoadedMetadata={onLoadedMetadata}
      onTimeUpdate={(event) => {
        if (!positioned.current) return;
        reportClipProgress(session, {
          positionMs: Math.round(event.currentTarget.currentTime * 1000),
        });
      }}
      onPlay={() => reportClipProgress(session, { paused: false })}
      onPlaying={(event) => applyAppVolume(event.currentTarget)}
      onPause={() => reportClipProgress(session, { paused: true })}
      // The end of the clip is the end of the visit: back to the cover,
      // the track paused at the clip's position.
      onEnded={() => requestBackToAudio("ended")}
      onError={() => reportClipProgress(session, { failed: true })}
    />
  );
}

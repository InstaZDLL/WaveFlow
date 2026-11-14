import { useEffect, useLayoutEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { TriangleAlert } from "lucide-react";

import { playableVideoSrc } from "../../hooks/usePlayableVideo";
import { usePlayer } from "../../hooks/usePlayer";
import {
  attachClipVideo,
  clipState,
  detachClipVideo,
  isCurrentClipSession,
  markClipPositioned,
  playClipElement,
  reportClipProgress,
  requestBackToAudio,
  useClipState,
} from "../../lib/clipPlayer";

/** Stop the clip this far before its end when the track's position would
 *  put it past it, so switching late in a song still shows a frame. */
const END_MARGIN_MS = 1000;

/** The app's volume, 0–1, kept by the surface showing the clip. */
let appVolume = 1;

/**
 * The session's `<video>`, created once and moved from one surface to the
 * other. A surface of its own used to mean an element of its own: enlarging
 * the clip unmounted the panel's and mounted a new one in the immersive
 * view, which loaded the file again (a second loopback request on Linux),
 * waited for its metadata and sought to the position — about a second of
 * silence and black screen. Moving the element keeps its decoder, its
 * buffer and its sound: a media element taken out of the document and put
 * back before the next task does not pause.
 */
let owned: { session: number; element: HTMLVideoElement } | null = null;

function release(element: HTMLVideoElement) {
  element.pause();
  element.removeAttribute("src");
  element.load();
}

function sessionElement(session: number, localPath: string): HTMLVideoElement {
  if (owned?.session === session) return owned.element;
  if (owned) release(owned.element);
  const element = document.createElement("video");
  owned = { session, element };
  // No browser controls, picture-in-picture or menu: they offer saving the
  // file and controls WaveFlow does not have.
  element.playsInline = true;
  element.disablePictureInPicture = true;
  element.className = "h-full w-full bg-black object-contain";
  element.volume = appVolume;
  element.addEventListener("contextmenu", (event) => event.preventDefault());

  // Until its metadata loads and it is put at the clip's position, the
  // element's time is 0 and says nothing about where the clip is.
  let positioned = false;
  element.addEventListener("loadedmetadata", () => {
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
    positioned = true;
    markClipPositioned(session, element);
    reportClipProgress(session, { durationMs, positionMs: target, aspect });
    // Autoplay with sound can be refused by the webview: the store then
    // says paused, and the player bar's Play button starts it.
    if (!paused) playClipElement(session, element);
  });
  element.addEventListener("timeupdate", () => {
    if (!positioned) return;
    reportClipProgress(session, {
      positionMs: Math.round(element.currentTime * 1000),
    });
  });
  element.addEventListener("play", () =>
    reportClipProgress(session, { paused: false }),
  );
  // Set again once the webview has opened its audio stream. WebKitGTK's
  // stream is restored by the sound server to the level it last had under
  // the app's name (PipeWire remembers it per application), and the
  // element takes that level over: a clip once left at 0 came back silent
  // at every later launch, the slider at 100 or not.
  element.addEventListener("playing", () => {
    if (element.volume !== appVolume) element.volume = appVolume;
  });
  element.addEventListener("pause", () =>
    reportClipProgress(session, { paused: true }),
  );
  // The end of the clip is the end of the visit: back to the cover, the
  // track paused at the clip's position.
  element.addEventListener("ended", () => requestBackToAudio("ended"));
  element.addEventListener("error", () =>
    reportClipProgress(session, { failed: true }),
  );

  void playableVideoSrc(localPath, false).then((src) => {
    if (owned?.element !== element) return;
    if (src) element.src = src;
    else reportClipProgress(session, { failed: true });
  });
  return element;
}

/**
 * The music video (#1073), in whichever surface shows it — the now-playing
 * panel or the immersive view. Driven by WaveFlow's own controls through
 * [`clipPlayer`](../../lib/clipPlayer.ts): no browser controls, no
 * picture-in-picture, no browser menu, so what is offered does not depend
 * on the webview and offers nothing WaveFlow does not do (downloading the
 * file, a playback rate of its own).
 *
 * Every opening is a session with one `<video>` of its own (the same clip
 * again, or a new one after one that failed), and the surface showing it
 * adopts that element rather than rendering one: a move from the panel to
 * the immersive view and back carries the playback itself across.
 */
export function ClipVideo({ className }: { className?: string }) {
  const clip = useClipState().clip;
  if (!clip) return null;
  return (
    <ClipSlot
      key={clip.session}
      session={clip.session}
      localPath={clip.localPath}
      className={className}
    />
  );
}

function ClipSlot({
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
  const { failed } = useClipState();
  const slot = useRef<HTMLDivElement>(null);

  // A layout effect, so the surface giving way and the one taking over act
  // in the same commit: the element is out of the document only between
  // the two, never across a task, and does not pause.
  useLayoutEffect(() => {
    const host = slot.current;
    if (!host) return;
    const element = sessionElement(session, localPath);
    host.appendChild(element);
    attachClipVideo(session, element);
    // A webview that paused it anyway (a surface that mounted later than
    // the one that let go) picks up again where the clip was going.
    if (
      element.paused &&
      element.readyState > 0 &&
      !clipState().paused &&
      isCurrentClipSession(session)
    ) {
      playClipElement(session, element);
    }
    return () => {
      detachClipVideo(session, element);
      // Released only when no surface took it over: the clip closed, or
      // another one opened.
      window.setTimeout(() => {
        if (!element.isConnected && owned?.element === element) {
          owned = null;
          release(element);
        }
      }, 0);
    };
  }, [session, localPath, failed]);

  // The app's volume, followed while the clip plays: the player bar's
  // slider is the one the user reaches for.
  useEffect(() => {
    appVolume = Math.min(1, Math.max(0, volume / 100));
    if (owned?.session === session) owned.element.volume = appVolume;
  }, [volume, session]);

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
  return <div ref={slot} className={`bg-black ${className ?? ""}`} />;
}

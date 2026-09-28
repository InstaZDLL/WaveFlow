import { convertFileSrc } from "@tauri-apps/api/core";
import { type RefObject, useEffect, useState } from "react";

import { getLocalVideoBaseUrl } from "../lib/tauri/canvas";

/**
 * WebKitGTK plays `<video>` through GStreamer, and neither of the obvious
 * ways to reach a local file works there:
 *
 * - the asset protocol has no GStreamer source at all ("no URI handler
 *   implemented for asset");
 * - a `blob:` URL corrupts a fragmented MP4 and errors on a large one;
 * - MediaSource never finishes appending one.
 *
 * So on Linux a local clip streams from the loopback server in
 * `media_loopback.rs`, which also rewrites a **fragmented** MP4 (Apple's
 * motion covers: `moof`/`mdat` pairs) as an ordinary one the first time it
 * is asked for it: over HTTP WebKit stops a fragmented file a couple of
 * seconds in and never resumes. WebView2 and WKWebView play the asset URL
 * directly.
 */
const LINUX =
  /linux/i.test(navigator.userAgent) && !/android/i.test(navigator.userAgent);

/** Asked once per launch: the server's port and token do not change. */
let loopbackBase: Promise<string | null> | null = null;
function loopbackBaseUrl(): Promise<string | null> {
  loopbackBase ??= getLocalVideoBaseUrl().catch((err: unknown) => {
    console.warn("[usePlayableVideo] no loopback server", err);
    return null;
  });
  return loopbackBase;
}

/**
 * Point `ref`'s `<video>` at a Canvas clip or a motion cover. `remote` URLs
 * (a plugin's, a server's ticketed one, an uncached motion cover) load as
 * they are; a local path goes through the asset protocol, or on Linux
 * through the loopback server as described above. Returns whether the clip
 * could not be set up — the caller shows the static cover. The element's
 * own `onError` still covers a clip that fails to decode.
 */
export function usePlayableVideo(
  ref: RefObject<HTMLVideoElement | null>,
  source: string,
  remote: boolean,
): { failed: boolean } {
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    const video = ref.current;
    if (!video) return;
    if (remote || !LINUX) {
      video.src = remote ? source : convertFileSrc(source);
      return;
    }

    let cancelled = false;
    void loopbackBaseUrl().then((base) => {
      if (cancelled) return;
      if (!base) {
        setFailed(true);
        return;
      }
      video.src = `${base}&path=${encodeURIComponent(source)}`;
    });

    return () => {
      cancelled = true;
      video.removeAttribute("src");
      video.load();
    };
  }, [ref, source, remote]);

  return { failed };
}

/** Whether looping clips need `useLoopFrameHold` (WebKitGTK only). */
export const HOLDS_LOOP_FRAME = LINUX;

/** How far into the new pass the held frame is dropped, in seconds. */
const RELEASE_AFTER = 0.5;

/**
 * Keep the last frame on screen while a looping `<video>` jumps back to
 * its start. WebKitGTK clears the picture during that seek and fetches the
 * start of the file again, so for a moment the element is transparent and
 * the static cover underneath flashes through. Each `timeupdate` (about
 * four a second) copies the current frame into `canvasRef`, drawn under
 * the video and over the cover; the seek shows it, and a moment into the
 * new pass it is hidden again. Being under the video, it hides nothing
 * once the video paints, so releasing it late costs nothing.
 *
 * The copy is not timed against `duration`: WebKitGTK reports the file's
 * full length but can loop well before it, where the download it paused
 * runs out (a fragmented MP4 over HTTP). Does nothing where the webview
 * keeps the frame itself.
 */
export function useLoopFrameHold(
  videoRef: RefObject<HTMLVideoElement | null>,
  canvasRef: RefObject<HTMLCanvasElement | null>,
): void {
  useEffect(() => {
    const video = videoRef.current;
    const canvas = canvasRef.current;
    if (!LINUX || !video || !canvas) return;
    let captured = false;
    let holding = false;

    const capture = () => {
      if (!video.videoWidth || !video.videoHeight) return;
      if (canvas.width !== video.videoWidth) canvas.width = video.videoWidth;
      if (canvas.height !== video.videoHeight) {
        canvas.height = video.videoHeight;
      }
      canvas.getContext("2d")?.drawImage(video, 0, 0);
      captured = true;
    };
    const onTimeUpdate = () => {
      if (!holding) {
        capture();
      } else if (!video.seeking && video.currentTime >= RELEASE_AFTER) {
        holding = false;
        canvas.style.opacity = "0";
      }
    };
    const onSeeking = () => {
      if (!video.loop || !captured) return;
      holding = true;
      canvas.style.opacity = "1";
    };

    video.addEventListener("timeupdate", onTimeUpdate);
    video.addEventListener("seeking", onSeeking);
    return () => {
      video.removeEventListener("timeupdate", onTimeUpdate);
      video.removeEventListener("seeking", onSeeking);
    };
  }, [videoRef, canvasRef]);
}

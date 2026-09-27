import { convertFileSrc } from "@tauri-apps/api/core";
import { type RefObject, useEffect, useState } from "react";

import { hasSoundTrack, isFragmentedMp4, mp4VideoMime } from "../lib/mp4Boxes";
import { getLocalVideoBaseUrl } from "../lib/tauri/canvas";

/**
 * WebKitGTK plays `<video>` through GStreamer, and neither of the obvious
 * ways to reach a local file works there for every clip:
 *
 * - the asset protocol has no GStreamer source at all ("no URI handler
 *   implemented for asset");
 * - over HTTP — the loopback server in `media_loopback.rs`, or Apple's own
 *   CDN — an ordinary MP4 plays, but a **fragmented** one (Apple's motion
 *   covers: `moof`/`mdat` pairs) stops after a couple of seconds, WebKit
 *   suspending the download and never resuming it;
 * - a `blob:` URL corrupts a fragmented MP4 outright.
 *
 * A fragmented MP4 is exactly what MediaSource consumes, so on Linux a
 * fragmented local file is read whole through the asset protocol and
 * appended to a `SourceBuffer`; an ordinary one streams from the loopback
 * server. WebView2 and WKWebView play the asset URL directly.
 */
const LINUX =
  /linux/i.test(navigator.userAgent) && !/android/i.test(navigator.userAgent);

/** Enough of the file to hold its `moov` box. */
const HEAD_BYTES = 256 * 1024;

/** Asked once per launch: the server's port and token do not change. */
let loopbackBase: Promise<string | null> | null = null;
function loopbackBaseUrl(): Promise<string | null> {
  loopbackBase ??= getLocalVideoBaseUrl().catch((err: unknown) => {
    console.warn("[usePlayableVideo] no loopback server", err);
    return null;
  });
  return loopbackBase;
}

function once(target: EventTarget, type: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const onError = () => reject(new Error(`${type}: error event`));
    target.addEventListener(
      type,
      () => {
        target.removeEventListener("error", onError);
        resolve();
      },
      { once: true },
    );
    target.addEventListener("error", onError, { once: true });
  });
}

async function readBytes(
  url: string,
  signal: AbortSignal,
  range?: string,
): Promise<ArrayBuffer> {
  const response = await fetch(url, {
    signal,
    headers: range ? { Range: range } : undefined,
  });
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return response.arrayBuffer();
}

/**
 * Point `ref`'s `<video>` at a Canvas clip or a motion cover. `remote` URLs
 * (a plugin's, a server's ticketed one, an uncached motion cover) load as
 * they are; a local path goes through the asset protocol, or on Linux
 * through MediaSource or the loopback server as described above. Returns
 * whether the clip could not be set up — the caller shows the static cover.
 * The element's own `onError` still covers a clip that fails to decode.
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

    const controller = new AbortController();
    let objectUrl: string | null = null;
    const asset = convertFileSrc(source);

    const attach = async () => {
      const head = new Uint8Array(
        await readBytes(asset, controller.signal, `bytes=0-${HEAD_BYTES - 1}`),
      );
      const mime =
        isFragmentedMp4(head) && !hasSoundTrack(head)
          ? mp4VideoMime(head)
          : null;
      if (!mime || !MediaSource.isTypeSupported(mime)) {
        const base = await loopbackBaseUrl();
        if (!base) throw new Error("no loopback server");
        if (controller.signal.aborted) return;
        video.src = `${base}&path=${encodeURIComponent(source)}`;
        return;
      }
      const bytes = await readBytes(asset, controller.signal);
      if (controller.signal.aborted) return;
      const mediaSource = new MediaSource();
      objectUrl = URL.createObjectURL(mediaSource);
      video.src = objectUrl;
      await once(mediaSource, "sourceopen");
      const buffer = mediaSource.addSourceBuffer(mime);
      const appended = once(buffer, "updateend");
      buffer.appendBuffer(bytes);
      await appended;
      if (mediaSource.readyState === "open") mediaSource.endOfStream();
    };

    attach().catch((err: unknown) => {
      if (controller.signal.aborted) return;
      console.warn("[usePlayableVideo] could not set up the clip", source, err);
      setFailed(true);
    });

    return () => {
      controller.abort();
      video.removeAttribute("src");
      video.load();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [ref, source, remote]);

  return { failed };
}

/** Whether looping clips need `useLoopFrameHold` (WebKitGTK only). */
export const HOLDS_LOOP_FRAME = LINUX;

/** How close to the end the last frame starts being kept, in seconds. */
const HOLD_WINDOW = 0.6;

/**
 * Keep the last frame on screen while a looping `<video>` jumps back to
 * its start. WebKitGTK clears the picture during that seek and fetches the
 * start of the file again, so for a moment the element is transparent and
 * the static cover underneath flashes through. Near the end, the current
 * frame is copied into `canvasRef` (drawn under the video, over the
 * cover); the seek shows it, and the first frame after it hides it again.
 * Does nothing where the webview keeps the frame itself.
 */
export function useLoopFrameHold(
  videoRef: RefObject<HTMLVideoElement | null>,
  canvasRef: RefObject<HTMLCanvasElement | null>,
): void {
  useEffect(() => {
    const video = videoRef.current;
    const canvas = canvasRef.current;
    if (!LINUX || !video || !canvas) return;
    let holding = false;

    const capture = () => {
      if (!video.videoWidth || !video.videoHeight) return;
      if (canvas.width !== video.videoWidth) canvas.width = video.videoWidth;
      if (canvas.height !== video.videoHeight) {
        canvas.height = video.videoHeight;
      }
      canvas.getContext("2d")?.drawImage(video, 0, 0);
    };
    const onTimeUpdate = () => {
      const { currentTime, duration } = video;
      // Back in the first half after the jump: the video has a frame again,
      // however late this event came.
      if (holding && currentTime > 0 && currentTime < duration / 2) {
        holding = false;
        canvas.style.opacity = "0";
      } else if (
        Number.isFinite(duration) &&
        duration - currentTime < HOLD_WINDOW
      ) {
        capture();
      }
    };
    const onSeeking = () => {
      if (!video.loop || canvas.width === 0) return;
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

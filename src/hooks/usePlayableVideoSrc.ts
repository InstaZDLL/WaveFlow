import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";

import { getLocalVideoBaseUrl } from "../lib/tauri/canvas";

/**
 * WebKitGTK plays `<video>` through GStreamer, which has no source for
 * Tauri's asset protocol ("no URI handler implemented for asset"), and a
 * `blob:` URL corrupts a fragmented MP4 — the form Apple's motion covers
 * come in. Its HTTP source handles both, so on Linux a local clip plays
 * through the loopback server in `media_loopback.rs`. WebView2 and
 * WKWebView play the asset URL directly.
 */
const VIA_LOOPBACK =
  /linux/i.test(navigator.userAgent) && !/android/i.test(navigator.userAgent);

/** Asked once per launch: the server's port and token do not change. */
let loopbackBase: Promise<string | null> | null = null;
function loopbackBaseUrl(): Promise<string | null> {
  loopbackBase ??= getLocalVideoBaseUrl().catch((err: unknown) => {
    console.warn("[usePlayableVideoSrc] no loopback server", err);
    return null;
  });
  return loopbackBase;
}

export interface PlayableVideoSrc {
  /** What to put in `<video src>`; `null` while the base URL resolves. */
  src: string | null;
  /** No way to play this local clip — show the static cover instead. */
  failed: boolean;
}

/**
 * Resolve a Canvas clip or a motion cover to a URL the webview's video
 * element can play. `remote` URLs (a plugin's, a server's ticketed one, an
 * uncached motion cover) load as they are; a local path goes through the
 * asset protocol, or on Linux through the loopback server.
 */
export function usePlayableVideoSrc(
  source: string,
  remote: boolean,
): PlayableVideoSrc {
  const viaLoopback = !remote && VIA_LOOPBACK;
  // `undefined` until the base URL has resolved, `null` if there is none.
  const [base, setBase] = useState<string | null | undefined>(undefined);

  useEffect(() => {
    if (!viaLoopback) return;
    let cancelled = false;
    void loopbackBaseUrl().then((url) => {
      if (!cancelled) setBase(url);
    });
    return () => {
      cancelled = true;
    };
  }, [viaLoopback]);

  if (remote) return { src: source, failed: false };
  if (!viaLoopback) return { src: convertFileSrc(source), failed: false };
  if (base === undefined) return { src: null, failed: false };
  if (base === null) return { src: null, failed: true };
  return { src: `${base}&path=${encodeURIComponent(source)}`, failed: false };
}

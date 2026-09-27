import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";

/**
 * WebKitGTK plays `<video>` through GStreamer, and GStreamer reads only the
 * schemes it has a source element for — `http(s)`, `file`, `blob:`. Tauri's
 * asset protocol is none of them: WebKit refuses it outright ("Requested
 * protocol: asset (allowed: no)"), and allowing it through
 * `WEBKIT_GST_ALLOWED_URI_PROTOCOLS` only moves the failure to "no URI
 * handler implemented for asset". So on Linux a local clip is fetched
 * through the asset protocol — the network stack does serve it — and handed
 * to the `<video>` as a `blob:` URL instead. WebView2 and WKWebView play the
 * asset URL directly.
 */
const NEEDS_BLOB =
  /linux/i.test(navigator.userAgent) && !/android/i.test(navigator.userAgent);

export interface PlayableVideoSrc {
  /** What to put in `<video src>`; `null` while a blob is being read. */
  src: string | null;
  /** The local file could not be read — show the static cover instead. */
  failed: boolean;
}

interface BlobState {
  /** The asset URL this state was produced for. */
  from: string;
  url: string | null;
  failed: boolean;
}

/**
 * Resolve a Canvas clip or a motion cover to a URL the webview's video
 * element can play. `remote` URLs (a plugin's, a server's ticketed one, an
 * uncached motion cover) load as they are; a local path goes through the
 * asset protocol, and on Linux through a `blob:` URL revoked on unmount.
 */
export function usePlayableVideoSrc(
  source: string,
  remote: boolean,
): PlayableVideoSrc {
  const direct = remote ? source : convertFileSrc(source);
  const viaBlob = !remote && NEEDS_BLOB;
  const [blob, setBlob] = useState<BlobState | null>(null);

  useEffect(() => {
    if (!viaBlob) return;
    let cancelled = false;
    let objectUrl: string | null = null;
    fetch(direct)
      .then((response) => {
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        return response.blob();
      })
      .then((data) => {
        if (cancelled) return;
        objectUrl = URL.createObjectURL(data);
        setBlob({ from: direct, url: objectUrl, failed: false });
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        console.warn(
          "[usePlayableVideoSrc] could not read the clip",
          direct,
          err,
        );
        setBlob({ from: direct, url: null, failed: true });
      });
    return () => {
      cancelled = true;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [direct, viaBlob]);

  if (!viaBlob) return { src: direct, failed: false };
  // A state left over from a previous source is not this one's answer.
  if (!blob || blob.from !== direct) return { src: null, failed: false };
  return { src: blob.url, failed: blob.failed };
}

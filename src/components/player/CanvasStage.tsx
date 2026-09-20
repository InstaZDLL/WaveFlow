import { useState } from "react";

import { convertFileSrc } from "@tauri-apps/api/core";

import { isRemoteCanvasUrl } from "../../lib/tauri/canvas";

const ROUND: Record<"md" | "lg" | "xl" | "2xl", string> = {
  md: "rounded-md",
  lg: "rounded-lg",
  xl: "rounded-xl",
  "2xl": "rounded-2xl",
};

/**
 * Per-track Canvas stage (issue #442). Drop it as a sibling of an
 * `<Artwork>` inside a `relative` container: when the current track has a
 * Canvas AND the global "Show Canvas" toggle is on AND the user hasn't asked
 * for reduced motion, a looping muted `<video>` fades in and **cleanly
 * replaces** the static cover; otherwise nothing renders and the cover shows
 * through. Spotify-style — the clip fills the cover frame (`object-cover`),
 * no blurred backdrop.
 *
 * The video is decorative (`aria-hidden`) — the accessible name lives on the
 * `<Artwork>` it sits over. A load/playback error silently falls back to the
 * static cover.
 *
 * A surface that wants to give the clip its own frame rather than the
 * cover's square one (issue #694) passes `onAspect` and sizes itself from
 * the answer.
 */
export function CanvasStage({
  path,
  enabled,
  rounded = "2xl",
  className,
  onAspect,
}: {
  /** Canvas source from `useTrackCanvas`: a **local** mp4 path (manual
   *  Canvas) OR a **remote** `https` URL (a `canvas`-world plugin, issue
   *  #473), or `null` when the track has none. */
  path: string | null;
  /** Global "Show Canvas" preference AND reduced-motion gate, resolved by the
   *  surface. When false the stage renders nothing. */
  enabled: boolean;
  rounded?: "md" | "lg" | "xl" | "2xl";
  className?: string;
  /**
   * The clip's own aspect ratio (width / height) once it can play, or
   * `null` when it turned out to be unplayable. **Tagged with the `path`
   * it describes** so a surface can ignore an answer about a clip it has
   * already moved off — the alternative, clearing on unmount, needs a
   * cleanup whose ordering against the next clip's mount is a coin toss.
   *
   * Fired on `canplay`, not `loadedmetadata`: the frame changing shape and
   * the video fading in are one movement, and metadata lands early enough
   * that splitting them shows the square cover inside an already-tall box.
   */
  onAspect?: (path: string, aspect: number | null) => void;
}) {
  if (!enabled || !path) return null;
  // Key on the path so switching track remounts the video and resets the
  // ready/failed state below.
  return (
    <CanvasVideo
      key={path}
      path={path}
      rounded={rounded}
      className={className}
      onAspect={onAspect}
    />
  );
}

function CanvasVideo({
  path,
  rounded,
  className,
  onAspect,
}: {
  path: string;
  rounded: "md" | "lg" | "xl" | "2xl";
  className?: string;
  onAspect?: (path: string, aspect: number | null) => void;
}) {
  const [ready, setReady] = useState(false);
  const [failed, setFailed] = useState(false);

  if (failed) return null;

  // A manual Canvas is a local file the webview can only reach through the
  // asset protocol; a plugin's (issue #473) and a server track's ticketed one
  // are already URLs the `<video>` loads directly — same split as
  // MotionCoverOverlay.
  const src = isRemoteCanvasUrl(path) ? path : convertFileSrc(path);

  return (
    <video
      src={src}
      autoPlay
      loop
      muted
      playsInline
      aria-hidden="true"
      onCanPlay={(e) => {
        setReady(true);
        const el = e.currentTarget;
        onAspect?.(
          path,
          el.videoHeight > 0 ? el.videoWidth / el.videoHeight : null,
        );
      }}
      onError={() => {
        setFailed(true);
        onAspect?.(path, null);
      }}
      className={`pointer-events-none absolute inset-0 w-full h-full object-cover ${ROUND[rounded]} transition-opacity duration-700 ${ready ? "opacity-100" : "opacity-0"} ${className ?? ""}`}
    />
  );
}

import { useState } from "react";

import { convertFileSrc } from "@tauri-apps/api/core";

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
 */
export function CanvasStage({
  path,
  enabled,
  rounded = "2xl",
  className,
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
    />
  );
}

function CanvasVideo({
  path,
  rounded,
  className,
}: {
  path: string;
  rounded: "md" | "lg" | "xl" | "2xl";
  className?: string;
}) {
  const [ready, setReady] = useState(false);
  const [failed, setFailed] = useState(false);

  if (failed) return null;

  // A manual Canvas is a local file the webview can only reach through the
  // asset protocol; a plugin-sourced one (issue #473) is already a remote
  // `https` URL the `<video>` loads directly — same split as MotionCoverOverlay.
  const src = /^https?:\/\//i.test(path) ? path : convertFileSrc(path);

  return (
    <video
      src={src}
      autoPlay
      loop
      muted
      playsInline
      aria-hidden="true"
      onCanPlay={() => setReady(true)}
      onError={() => setFailed(true)}
      className={`pointer-events-none absolute inset-0 w-full h-full object-cover ${ROUND[rounded]} transition-opacity duration-700 ${ready ? "opacity-100" : "opacity-0"} ${className ?? ""}`}
    />
  );
}

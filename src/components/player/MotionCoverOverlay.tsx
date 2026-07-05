import { useState } from "react";

import { useAlbumMotionArtwork } from "../../hooks/useAlbumMotionArtwork";

const ROUND: Record<"md" | "lg" | "xl" | "2xl", string> = {
  md: "rounded-md",
  lg: "rounded-lg",
  xl: "rounded-xl",
  "2xl": "rounded-2xl",
};

/**
 * Animated-album-artwork overlay (Phase 3). Drop it as a sibling of an
 * `<Artwork>` inside a `relative` container: when an enabled metadata
 * plugin has a motion cover for the current album, a looping muted
 * `<video>` fades in over the static cover; otherwise nothing renders and
 * the cover shows through. A load / playback error silently falls back to
 * the static cover (the `<Artwork>` underneath is always painted).
 *
 * The video is decorative (`aria-hidden`) — the accessible name lives on
 * the `<Artwork>` it sits over.
 */
export function MotionCoverOverlay({
  artist,
  album,
  rounded = "2xl",
  className,
}: {
  artist: string | null | undefined;
  album: string | null | undefined;
  rounded?: "md" | "lg" | "xl" | "2xl";
  className?: string;
}) {
  const motion = useAlbumMotionArtwork(artist, album);
  if (!motion) return null;
  // Key on the URL so switching album remounts the video and resets the
  // ready/failed state below.
  return (
    <MotionVideo
      key={motion.squareUrl}
      url={motion.squareUrl}
      rounded={rounded}
      className={className}
    />
  );
}

function MotionVideo({
  url,
  rounded,
  className,
}: {
  url: string;
  rounded: "md" | "lg" | "xl" | "2xl";
  className?: string;
}) {
  const [ready, setReady] = useState(false);
  const [failed, setFailed] = useState(false);

  if (failed) return null;

  return (
    <video
      src={url}
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

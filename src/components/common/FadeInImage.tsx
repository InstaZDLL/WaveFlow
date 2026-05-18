import { useState } from "react";

interface FadeInImageProps {
  /** Resolved `asset://` (or http) URL. */
  src: string;
  alt: string;
  /** Tailwind classes for the wrapper that hosts the placeholder
   *  gradient and clips the image. */
  wrapperClassName: string;
  /** Tailwind classes for the `<img>` itself. Should NOT carry sizing
   *  — the wrapper handles that. */
  imgClassName?: string;
  /** Optional placeholder children rendered behind the image (e.g. a
   *  fallback initial or a Disc icon). Hidden once the image fades in. */
  placeholder?: React.ReactNode;
}

/**
 * Render an `<img>` that fades in over an underlying placeholder once
 * the browser has decoded it. Replaces the classic "flash from grey
 * to image" you get when a whole grid of fresh thumbnails mounts and
 * each tile pops in independently.
 *
 * Why a wrapper instead of just a CSS transition: the image hasn't
 * decoded yet on first paint, so we have to gate `opacity` on the
 * `onLoad` event. We also handle the WebView-cached case via `ref`,
 * where `complete` is already true and `onLoad` would never fire.
 */
export function FadeInImage({
  src,
  alt,
  wrapperClassName,
  imgClassName = "",
  placeholder,
}: FadeInImageProps) {
  // Reset the fade gate when the underlying URL changes (e.g. a tile
  // gets recycled by a virtualized list, or the same component
  // re-renders with a different artist). React's documented pattern
  // for "reset state when prop changes" is to compare against a prev
  // ref in render rather than chaining a useEffect — avoids the
  // double-render that eslint-plugin-react-hooks flags.
  const [loaded, setLoaded] = useState(false);
  const [prevSrc, setPrevSrc] = useState(src);
  if (prevSrc !== src) {
    setPrevSrc(src);
    setLoaded(false);
  }

  return (
    <div className={`relative overflow-hidden ${wrapperClassName}`}>
      {placeholder ? (
        <div
          aria-hidden
          className={`absolute inset-0 flex items-center justify-center transition-opacity duration-200 ${
            loaded ? "opacity-0" : "opacity-100"
          }`}
        >
          {placeholder}
        </div>
      ) : null}
      <img
        src={src}
        alt={alt}
        loading="lazy"
        decoding="async"
        draggable={false}
        onLoad={() => setLoaded(true)}
        ref={(el) => {
          // The WebView cache can serve the bytes synchronously — the
          // <img> is already `complete` before React even attaches an
          // onLoad handler. Pin the fade open from the ref callback in
          // that case so we don't stay stuck at opacity-0.
          if (el && el.complete && el.naturalWidth > 0) {
            setLoaded(true);
          }
        }}
        className={`absolute inset-0 w-full h-full object-cover transition-opacity duration-200 ${
          loaded ? "opacity-100" : "opacity-0"
        } ${imgClassName}`}
      />
    </div>
  );
}

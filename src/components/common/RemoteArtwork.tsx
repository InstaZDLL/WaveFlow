import { ListMusic } from "lucide-react";
import { useRemoteArtworkSrc } from "../../hooks/useRemoteArtworkSrc";

export function RemoteArtwork({
  hash,
  className = "w-9 h-9 rounded",
  iconSize = 14,
}: {
  hash: string | null;
  className?: string;
  iconSize?: number;
}) {
  const { src, onError, onLoad } = useRemoteArtworkSrc(hash);
  if (!src) {
    return (
      <div
        className={`${className} bg-zinc-200 dark:bg-zinc-700 flex items-center justify-center shrink-0`}
      >
        <ListMusic size={iconSize} className="text-zinc-400" />
      </div>
    );
  }
  return (
    <img
      src={src}
      alt=""
      onError={onError}
      onLoad={onLoad}
      className={`${className} object-cover shrink-0`}
      loading="lazy"
    />
  );
}

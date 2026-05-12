import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Maximize2, MoreHorizontal, PictureInPicture2 } from "lucide-react";

interface MoreActionsMenuProps {
  /** When `false`, the mini-player entry is hidden — used in Spotify
   *  mode where the WebPlayer SDK can't drive a second webview. */
  miniPlayerAvailable: boolean;
  onOpenFullscreen: () => void;
  onOpenMiniPlayer: () => void;
}

/**
 * Overflow popover that absorbs the player bar's secondary actions
 * (Fullscreen, Mini-player). Trigger is a single "⋯" icon so the
 * crowded bottom-right cluster doesn't keep growing each time we
 * add a feature. Lyrics / Queue / Device / Volume stay first-class
 * because they're the most-used controls.
 */
export function MoreActionsMenu({
  miniPlayerAvailable,
  onOpenFullscreen,
  onOpenMiniPlayer,
}: MoreActionsMenuProps) {
  const { t } = useTranslation();
  const [isOpen, setIsOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!isOpen) return;
    const onPointer = (event: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(event.target as Node)
      ) {
        setIsOpen(false);
      }
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setIsOpen(false);
    };
    document.addEventListener("mousedown", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [isOpen]);

  const handle = (cb: () => void) => () => {
    setIsOpen(false);
    cb();
  };

  return (
    <div ref={containerRef} className="relative">
      <button
        type="button"
        onClick={() => setIsOpen((open) => !open)}
        aria-label={t("playerBar.moreActions")}
        aria-haspopup="menu"
        aria-expanded={isOpen}
        title={t("playerBar.moreActions")}
        className={`p-2 rounded-lg transition-colors ${
          isOpen
            ? "text-emerald-500"
            : "text-zinc-400 hover:text-zinc-800 dark:hover:text-white"
        }`}
      >
        <MoreHorizontal size={20} />
      </button>

      {isOpen && (
        <div
          role="menu"
          aria-label={t("playerBar.moreActions")}
          className="absolute bottom-full right-0 mb-3 w-56 p-1 rounded-xl bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 shadow-xl z-50"
        >
          <button
            type="button"
            role="menuitem"
            onClick={handle(onOpenFullscreen)}
            className="w-full flex items-center gap-3 px-3 py-2 text-sm text-zinc-700 dark:text-zinc-200 rounded-lg hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
          >
            <Maximize2 size={16} className="text-zinc-500" />
            {t("playerBar.openFullscreen")}
          </button>
          {miniPlayerAvailable && (
            <button
              type="button"
              role="menuitem"
              onClick={handle(onOpenMiniPlayer)}
              className="w-full flex items-center gap-3 px-3 py-2 text-sm text-zinc-700 dark:text-zinc-200 rounded-lg hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
            >
              <PictureInPicture2 size={16} className="text-zinc-500" />
              {t("playerBar.miniPlayer")}
            </button>
          )}
        </div>
      )}
    </div>
  );
}

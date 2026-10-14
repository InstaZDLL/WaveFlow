import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Check, Copy, Download, Loader2, Share2 } from "lucide-react";
import { useShareImage } from "../../hooks/useShareImage";
import {
  ContextMenu,
  ContextMenuItem,
  type ContextMenuPoint,
} from "./ContextMenu";

interface ShareImageButtonProps {
  /** Renders the card, only once the user picked save or copy. */
  buildCard: () => Promise<Blob>;
  /** Start of the default filename: the playlist or artist name. */
  fileStem: string;
  className: string;
  iconSize?: number;
  /** Shown beside the icon; icon only when absent. */
  label?: ReactNode;
}

/**
 * "Share" for a playlist or artist header: opens a menu with "Save as PNG"
 * and "Copy image". The menu is the app's portalled `ContextMenu`, so a
 * header with `overflow-hidden` or its own stacking context cannot clip it.
 */
export function ShareImageButton({
  buildCard,
  fileStem,
  className,
  iconSize = 18,
  label,
}: ShareImageButtonProps) {
  const { t } = useTranslation();
  const share = useShareImage(buildCard, fileStem);
  const [menuAt, setMenuAt] = useState<ContextMenuPoint | null>(null);

  const icon = share.busy ? (
    <Loader2 size={iconSize} className="animate-spin" />
  ) : share.state === "done" ? (
    <Check size={iconSize} />
  ) : (
    <Share2 size={iconSize} />
  );

  return (
    <>
      <button
        type="button"
        onClick={(e) => {
          const r = e.currentTarget.getBoundingClientRect();
          setMenuAt({ x: r.left, y: r.bottom + 6 });
        }}
        disabled={share.busy}
        aria-label={t("nowPlaying.share.open")}
        aria-haspopup="menu"
        className={className}
      >
        {icon}
        {label != null && <span>{label}</span>}
      </button>
      {menuAt && (
        <ContextMenu point={menuAt} onClose={() => setMenuAt(null)}>
          <ContextMenuItem
            icon={<Download size={14} />}
            label={t("nowPlaying.share.save")}
            onSelect={() => {
              setMenuAt(null);
              void share.save();
            }}
          />
          <ContextMenuItem
            icon={<Copy size={14} />}
            label={t("nowPlaying.share.copy")}
            onSelect={() => {
              setMenuAt(null);
              void share.copy();
            }}
          />
        </ContextMenu>
      )}
    </>
  );
}

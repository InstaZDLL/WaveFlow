import type { KeyboardEvent } from "react";
import { motion } from "framer-motion";
import { X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { usePlayer, type RightPanel } from "../../../hooks/usePlayer";
import { NowPlayingPanel } from "../NowPlayingPanel";
import { LyricsPanel } from "../LyricsPanel";
import { QueuePanel } from "../QueuePanel";

interface FluidSidePanelProps {
  onNavigateToArtist: (artistId: number) => void;
  onNavigateToRemoteArtist: (artistId: string) => void;
}

const TABS: RightPanel[] = ["nowPlaying", "lyrics", "queue"];

/**
 * The Fluid skin's right edge: one panel with three tabs — Details,
 * Lyrics, Queue — where the other skins mount three panels in turn.
 * The three bodies are the existing panels in their embedded form (no
 * frame of their own, no header, no close button): the data, the
 * actions and the keyboard contract are theirs, only the shell is new.
 * The player bar's buttons keep their meaning — each opens its tab, or
 * closes the panel when that tab is already up.
 */
export function FluidSidePanel({
  onNavigateToArtist,
  onNavigateToRemoteArtist,
}: FluidSidePanelProps) {
  const { t } = useTranslation();
  const { activeRightPanel, openRightPanel, closeRightPanel } = usePlayer();
  const labels: Record<RightPanel, string> = {
    nowPlaying: t("nowPlaying.fluid.details"),
    lyrics: t("lyrics.title"),
    queue: t("queue.title"),
  };

  // Roving tabs: arrows move, Home / End jump, the focused tab opens.
  const onTabKey = (e: KeyboardEvent<HTMLButtonElement>, index: number) => {
    let next: number | null = null;
    if (e.key === "ArrowRight") next = (index + 1) % TABS.length;
    else if (e.key === "ArrowLeft")
      next = (index - 1 + TABS.length) % TABS.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = TABS.length - 1;
    if (next == null) return;
    e.preventDefault();
    openRightPanel(TABS[next]);
    const list = e.currentTarget.parentElement;
    const target =
      list?.querySelectorAll<HTMLButtonElement>('[role="tab"]')[next];
    target?.focus();
  };

  return (
    <motion.aside
      key="fluid-side-panel"
      initial={{ width: 0, opacity: 0 }}
      animate={{ width: 340, opacity: 1 }}
      exit={{ width: 0, opacity: 0 }}
      transition={{ type: "spring", stiffness: 320, damping: 32, mass: 0.8 }}
      className="fe-panel h-full shrink-0 overflow-hidden"
    >
      <div className="flex h-full w-[340px] flex-col">
        <div className="flex items-center justify-between px-3 pb-1 pt-2.5">
          <div
            role="tablist"
            aria-label={t("immersive.panelTabs")}
            className="flex items-center gap-0.5"
          >
            {TABS.map((id, i) => {
              const active = activeRightPanel === id;
              return (
                <button
                  key={id}
                  type="button"
                  role="tab"
                  aria-selected={active}
                  tabIndex={active ? 0 : -1}
                  onClick={() => openRightPanel(id)}
                  onKeyDown={(e) => onTabKey(e, i)}
                  className={`fe-panel__tab ${active ? "is-active" : ""}`}
                >
                  {labels[id]}
                </button>
              );
            })}
          </div>
          <button
            type="button"
            onClick={closeRightPanel}
            aria-label={t("common.close")}
            className="fe-icon-btn"
          >
            <X size={16} />
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-hidden">
          {activeRightPanel === "nowPlaying" && (
            <NowPlayingPanel
              embedded
              onNavigateToArtist={onNavigateToArtist}
              onNavigateToRemoteArtist={onNavigateToRemoteArtist}
            />
          )}
          {activeRightPanel === "lyrics" && <LyricsPanel embedded />}
          {activeRightPanel === "queue" && <QueuePanel embedded />}
        </div>
      </div>
    </motion.aside>
  );
}

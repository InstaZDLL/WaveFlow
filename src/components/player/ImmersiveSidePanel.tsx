import { useTranslation } from "react-i18next";
import { Mic2, ListMusic } from "lucide-react";
import type { Track } from "../../lib/tauri/track";
import type { TrackLyrics } from "../../hooks/useTrackLyrics";
import { ImmersiveLyricsColumn } from "./ImmersiveLyricsColumn";
import { ImmersiveQueueTab } from "./ImmersiveQueueTab";

export type ImmersiveTab = "lyrics" | "queue";

interface ImmersiveSidePanelProps {
  track: Track;
  lyrics: TrackLyrics;
  activeTab: ImmersiveTab;
  onTabChange: (tab: ImmersiveTab) => void;
}

/**
 * Right-side control panel of the immersive view (issue #328 follow-up).
 * A segmented tab header switches the content between the synced lyrics
 * and the playback queue, so the right side is a small dashboard rather
 * than a single lyrics pane. Built to grow — add a tab id + a content
 * branch to extend it.
 */
export function ImmersiveSidePanel({
  track,
  lyrics,
  activeTab,
  onTabChange,
}: ImmersiveSidePanelProps) {
  const { t } = useTranslation();

  const tabs: Array<{ id: ImmersiveTab; label: string; icon: typeof Mic2 }> = [
    { id: "lyrics", label: t("lyrics.title"), icon: Mic2 },
    { id: "queue", label: t("queue.title"), icon: ListMusic },
  ];

  return (
    <div className="h-full flex flex-col text-white">
      {/* Segmented tab header */}
      <div className="shrink-0 px-6 pt-8 pb-4">
        <div
          role="tablist"
          aria-label={t("immersive.panelTabs")}
          className="inline-flex items-center gap-1 p-1 rounded-full bg-white/10"
        >
          {tabs.map(({ id, label, icon: Icon }) => {
            const active = activeTab === id;
            return (
              <button
                key={id}
                type="button"
                role="tab"
                aria-selected={active}
                onClick={() => onTabChange(id)}
                className={`inline-flex items-center gap-2 px-4 py-1.5 rounded-full text-sm font-medium transition-colors ${
                  active
                    ? "bg-white text-zinc-900"
                    : "text-white/70 hover:text-white"
                }`}
              >
                <Icon size={15} />
                {label}
              </button>
            );
          })}
        </div>
      </div>

      {/* Active tab content */}
      {activeTab === "lyrics" ? (
        <ImmersiveLyricsColumn
          track={track}
          payload={lyrics.payload}
          lrcLines={lyrics.lrcLines}
          isSynced={lyrics.isSynced}
          activeIndex={lyrics.activeIndex}
          activeWordIndex={lyrics.activeWordIndex}
          isFetching={lyrics.isFetching}
          error={lyrics.error}
          staticText={lyrics.radioPlainText}
          isRadio={lyrics.isRadio}
          onSeek={lyrics.seekToLine}
          onImport={() => void lyrics.importLyrics()}
          onRefetch={() => void lyrics.refetch()}
          showHeader={false}
        />
      ) : (
        <ImmersiveQueueTab />
      )}
    </div>
  );
}

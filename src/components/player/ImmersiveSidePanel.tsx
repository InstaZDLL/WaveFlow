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
    <div className="@container h-full flex flex-col text-white">
      {/* Tab header: icon buttons in the style of the view's own buttons,
          on the same line — `pt-6` matches the bar's `py-6`, and both are
          `p-2.5` round buttons with a 22px icon.

          The view's buttons (panel, Canvas, ⋯, share, close) float over
          the top right of this panel and take about 330px with the Canvas
          ones. The labelled pill this replaces ran under them in any panel
          narrower than 48rem, a 1080p window (#733); the fix for that
          dropped the pill below the buttons, which pushed the lyrics, and
          their focused line, down the screen (#750). Icons alone take
          about 120px, so both fit on one line down to a 32rem panel; only
          narrower than that do the tabs still step below. Each tab's name
          is its tooltip, as it is for every button beside it. Measured on
          the panel, not the screen: the panel is a share of the window. */}
      <div className="shrink-0 px-6 pt-6 @max-lg:pt-24 pb-4">
        <div
          role="tablist"
          aria-label={t("immersive.panelTabs")}
          className="inline-flex items-center gap-3"
        >
          {tabs.map(({ id, label, icon: Icon }) => {
            const active = activeTab === id;
            return (
              <button
                key={id}
                type="button"
                role="tab"
                aria-selected={active}
                aria-label={label}
                title={label}
                onClick={() => onTabChange(id)}
                className={`p-2.5 rounded-full transition-colors ${
                  active
                    ? "bg-white/25 text-white"
                    : "bg-white/10 hover:bg-white/20 text-white/80"
                }`}
              >
                <Icon size={22} />
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
          excludedGenre={lyrics.excludedGenre}
          staticText={lyrics.radioPlainText}
          // Hides the import / refetch CTA — neither applies without a
          // library row, which a remote-source stream also lacks.
          isRadio={lyrics.isRadio || lyrics.isRemote}
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

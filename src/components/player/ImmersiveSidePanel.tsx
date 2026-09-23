import { useLayoutEffect, useRef, useState, type RefObject } from "react";
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
  /** The view's floating top-right buttons, which sit over this panel's
   *  top line; measured to know how much of that line is free. */
  buttonBarRef: RefObject<HTMLDivElement | null>;
}

/**
 * How the tabs sit, chosen from the room actually left beside the view's
 * buttons:
 * - `pill` — the labelled pill, on the buttons' line;
 * - `icons` — round icon buttons on that line, when only they fit;
 * - `stacked` — the labelled pill below the buttons, when not even the
 *   icons fit.
 */
type TabLayout = "pill" | "icons" | "stacked";

/** Two `p-2.5` round buttons with a 22px icon (42px each) and `gap-3`. */
const ICON_TABS_WIDTH = 42 * 2 + 12;
/** Breathing room kept between the tabs and the first floating button. */
const LINE_GAP = 16;

/**
 * Right-side control panel of the immersive view (issue #328 follow-up).
 * A tab header switches the content between the synced lyrics and the
 * playback queue, so the right side is a small dashboard rather than a
 * single lyrics pane. Built to grow — add a tab id + a content branch to
 * extend it.
 */
export function ImmersiveSidePanel({
  track,
  lyrics,
  activeTab,
  onTabChange,
  buttonBarRef,
}: ImmersiveSidePanelProps) {
  const { t } = useTranslation();

  const tabs: Array<{ id: ImmersiveTab; label: string; icon: typeof Mic2 }> = [
    { id: "lyrics", label: t("lyrics.title"), icon: Mic2 },
    { id: "queue", label: t("queue.title"), icon: ListMusic },
  ];

  // The view's buttons (panel, Canvas, ⋯, share, close) float over this
  // panel's top right. A fixed breakpoint cannot tell whether the tabs fit
  // beside them: the Canvas buttons come and go with the track, and the
  // pill's width depends on the language ("File d'attente" is not "Play
  // queue"). A 48rem breakpoint dropped the pill below the buttons in a
  // panel that had room for it, which pushed the lyrics — and the focused
  // line they centre in their own scroller — down the screen (#733,
  // #750). So the room is measured: the pill when it fits, icons when only
  // they do, and below the buttons only as a last resort.
  //
  // Nothing measured depends on the layout chosen — the header row spans
  // the panel whatever it holds, the pill is measured from a hidden copy,
  // and the buttons are the parent's — so the choice cannot oscillate.
  const [layout, setLayout] = useState<TabLayout>("pill");
  const headerRef = useRef<HTMLDivElement>(null);
  const pillMeasureRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    const header = headerRef.current;
    const pill = pillMeasureRef.current;
    const bar = buttonBarRef.current;
    if (!header || !pill) return;

    const measure = () => {
      const headerBox = header.getBoundingClientRect();
      const style = getComputedStyle(header);
      const start = headerBox.left + parseFloat(style.paddingLeft);
      // The bar is padded; its leftmost button is where the room ends.
      const firstButton = bar?.firstElementChild;
      const end = firstButton
        ? firstButton.getBoundingClientRect().left
        : headerBox.right - parseFloat(style.paddingRight);
      const room = end - start - LINE_GAP;
      const pillWidth = pill.getBoundingClientRect().width;
      setLayout(
        pillWidth <= room
          ? "pill"
          : ICON_TABS_WIDTH <= room
            ? "icons"
            : "stacked",
      );
    };

    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(header);
    observer.observe(pill);
    if (bar) observer.observe(bar);
    return () => observer.disconnect();
  }, [buttonBarRef]);

  const pillTabs = (interactive: boolean) =>
    tabs.map(({ id, label, icon: Icon }) => {
      const active = activeTab === id;
      const className = `inline-flex items-center gap-2 px-4 py-1.5 rounded-full text-sm font-medium transition-colors ${
        active ? "bg-white text-zinc-900" : "text-white/70 hover:text-white"
      }`;
      return interactive ? (
        <button
          key={id}
          type="button"
          role="tab"
          aria-selected={active}
          onClick={() => onTabChange(id)}
          className={className}
        >
          <Icon size={15} />
          {label}
        </button>
      ) : (
        <span key={id} className={className}>
          <Icon size={15} />
          {label}
        </span>
      );
    });

  return (
    <div className="h-full flex flex-col text-white">
      {/* `pt-6` matches the view's bar (`py-6`) and the 42px row matches
          its buttons, so tabs on that line centre on it. */}
      <div
        ref={headerRef}
        className={`relative shrink-0 px-6 pb-4 ${
          layout === "stacked" ? "pt-24" : "pt-6"
        }`}
      >
        {/* Hidden copy of the pill: its width, whatever is shown. */}
        <div
          ref={pillMeasureRef}
          aria-hidden="true"
          className="invisible pointer-events-none absolute left-0 top-0 inline-flex items-center gap-1 p-1 rounded-full whitespace-nowrap"
        >
          {pillTabs(false)}
        </div>

        <div className="min-h-10.5 flex items-center">
          {layout === "icons" ? (
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
          ) : (
            <div
              role="tablist"
              aria-label={t("immersive.panelTabs")}
              className="inline-flex items-center gap-1 p-1 rounded-full bg-white/10 whitespace-nowrap"
            >
              {pillTabs(true)}
            </div>
          )}
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

import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { X, PanelRight, Mic2, MoreHorizontal } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import { usePlayer } from "../../hooks/usePlayer";
import { useTrackLyrics } from "../../hooks/useTrackLyrics";
import { useImmersivePrefs } from "../../hooks/useImmersivePrefs";
import { useTrackCanvas } from "../../hooks/useTrackCanvas";
import { useCanvasEnabled, setCanvasEnabled } from "../../hooks/useCanvasEnabled";
import { usePrefersReducedMotion } from "../../hooks/usePrefersReducedMotion";
import { isRadioTrack } from "../../lib/playerSources";
import { Artwork } from "../common/Artwork";
import { ImmersiveNowPlaying } from "./ImmersiveNowPlaying";
import { ImmersiveSidePanel, type ImmersiveTab } from "./ImmersiveSidePanel";
import { ImmersiveLyricsColumn } from "./ImmersiveLyricsColumn";
import { ImmersiveShareButton } from "./ImmersiveShareButton";
import { CanvasToggleButton } from "./CanvasToggleButton";
import { CanvasPickerModal } from "../common/CanvasPickerModal";

/** Below this width the dual-column layout collapses to a single column
 *  with a now-playing ⇄ panel toggle — two columns only make sense at
 *  desktop widths. */
const NARROW_BREAKPOINT = 900;

interface ImmersiveViewProps {
  /** Which entry point opened the view — only used to pick the first
   *  column in the narrow single-column fallback. */
  initialTab: "nowPlaying" | "lyrics";
  onClose: () => void;
  onNavigateToArtist: (artistId: number) => void;
  isLiked: boolean;
  onToggleLike: () => void;
}

/**
 * Immersive view (issue #328) — the now-playing cover/transport and the
 * synced lyrics merged into one fullscreen view so the user can switch
 * tracks while reading lyrics, with no empty side space on wide
 * displays. Replaces the old mutually-exclusive `FullscreenNowPlaying` /
 * `FullscreenLyrics` overlays.
 *
 * Two genuinely distinct interfaces, chosen by `immersive.merged_lyrics`
 * (forced to the classic one on narrow windows where two columns don't
 * fit):
 *  - **merged** (pref ON + wide window): now-playing on the left + a
 *    tabbed control panel ([`ImmersiveSidePanel`] — Lyrics / Queue) on
 *    the right; a `PanelRight` button hides/shows the panel.
 *  - **classic** (pref OFF or narrow window): now-playing fullscreen with
 *    a Mic2 button flipping to a lyrics-only fullscreen — the pre-#328
 *    behaviour, deliberately left intact (no panel, no queue).
 *
 * Native fullscreen (`immersive.use_native_fullscreen`, default ON)
 * drives the OS window into real fullscreen on open and restores the
 * prior window state on close. Escape closes the view; exiting OS
 * fullscreen another way (F11) is deliberately decoupled — it leaves the
 * view as a windowed overlay rather than dismissing it.
 */
export function ImmersiveView({
  initialTab,
  onClose,
  onNavigateToArtist,
  isLiked,
  onToggleLike,
}: ImmersiveViewProps) {
  const { t } = useTranslation();
  const { currentTrack, activeProvider } = usePlayer();
  const {
    mergedLyrics,
    useNativeFullscreen,
    loaded: prefsLoaded,
  } = useImmersivePrefs();
  const lyrics = useTrackLyrics();

  // Per-track Canvas (issue #442). Only a real library track can carry one
  // (radio uses a negative sentinel id, Spotify has no local row), so the
  // toggle + set/remove entry gate on that. The toggle only shows when the
  // track actually has a Canvas and motion isn't reduced — never a dead
  // control; the "⋯" set/remove entry shows for any eligible track.
  const canvasEnabled = useCanvasEnabled();
  const reducedMotion = usePrefersReducedMotion();
  const canvasPath = useTrackCanvas(currentTrack);
  const [canvasPickerOpen, setCanvasPickerOpen] = useState(false);
  // Freeze the target track + its Canvas state when the picker opens, so an
  // auto-advance to the next track mid-dialog can't redirect a set/remove
  // onto the wrong track. Kept after close for the exit animation; the next
  // open overwrites it with a fresh snapshot.
  const [canvasPickerTarget, setCanvasPickerTarget] = useState<{
    trackId: number;
    hasCanvas: boolean;
  } | null>(null);
  const canvasTrack =
    currentTrack &&
    activeProvider !== "spotify" &&
    !isRadioTrack(currentTrack) &&
    currentTrack.id >= 0
      ? currentTrack
      : null;
  const canvasAvailable = !!canvasPath && !reducedMotion;
  // The picker sets/removes the MANUAL local Canvas only; a plugin-sourced
  // Canvas (issue #473) is a remote `https` URL and must NOT read as
  // "has a Canvas to remove". Same http(s) split as CanvasStage.
  const hasManualCanvas = !!canvasPath && !/^https?:\/\//i.test(canvasPath);

  // Escape close + focus trap. Only mounted while open → pass `true`.
  const dialogRef = useModalA11y<HTMLDivElement>(true, onClose);

  // Track the viewport width so the dual layout collapses on narrow
  // windows. `window.innerWidth` is fine — the view is always
  // full-window so there's no inner container to measure.
  const [narrow, setNarrow] = useState(
    () => window.innerWidth < NARROW_BREAKPOINT,
  );
  useEffect(() => {
    const onResize = () => setNarrow(window.innerWidth < NARROW_BREAKPOINT);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  // Two genuinely distinct interfaces, chosen by the merged pref (and
  // forced to the classic one on narrow windows where two columns don't
  // fit):
  //  - `dual` → NEW merged view: now-playing + a tabbed control panel
  //    (Lyrics / Queue) on the right, toggled with a PanelRight button.
  //  - else  → CLASSIC view: now-playing fullscreen with a Mic2 button
  //    that flips to a lyrics-only fullscreen (the pre-#328 behaviour,
  //    unchanged — no panel, no queue).
  const dual = mergedLyrics && !narrow;

  // Dual: which tab the control panel shows + whether it's open.
  const [activeTab, setActiveTab] = useState<ImmersiveTab>("lyrics");
  const [panelOpen, setPanelOpen] = useState(true);
  // Classic: which of the two fullscreens is visible. Opening via the
  // lyrics button lands on lyrics.
  const [classicSide, setClassicSide] = useState<"nowPlaying" | "lyrics">(
    initialTab === "lyrics" ? "lyrics" : "nowPlaying",
  );

  const togglePanel = () => setPanelOpen((v) => !v);
  const toggleClassicLyrics = () =>
    setClassicSide((s) => (s === "lyrics" ? "nowPlaying" : "lyrics"));

  // ── Native fullscreen (reversible) ───────────────────────────────
  // Enter on mount, restore the prior window state on unmount. Refs so
  // the cleanup restores exactly what we changed even if mount/unmount
  // race a very fast open→close.
  const enteredFullscreenRef = useRef(false);
  const priorMaximizedRef = useRef(false);
  useEffect(() => {
    // Wait for the real pref before touching the OS window — acting on
    // the optimistic default would flash a user who disabled native
    // fullscreen into fullscreen for one tick.
    if (!prefsLoaded || !useNativeFullscreen) return;
    let cancelled = false;
    (async () => {
      try {
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const win = getCurrentWindow();
        const alreadyFullscreen = await win.isFullscreen();
        if (alreadyFullscreen || cancelled) return;
        priorMaximizedRef.current = await win.isMaximized();
        if (cancelled) return;
        await win.setFullscreen(true);
        // The cleanup may have fired while `setFullscreen(true)` was in
        // flight (fast open→close). It skips restoration because
        // `enteredFullscreenRef` was still false, so undo here and don't
        // claim ownership — otherwise the window stays fullscreen after
        // unmount.
        if (cancelled) {
          await win.setFullscreen(false);
          if (priorMaximizedRef.current) await win.maximize();
          return;
        }
        enteredFullscreenRef.current = true;
      } catch (err) {
        console.error("[ImmersiveView] enter fullscreen failed", err);
      }
    })();
    return () => {
      cancelled = true;
      if (!enteredFullscreenRef.current) return;
      enteredFullscreenRef.current = false;
      const wasMaximized = priorMaximizedRef.current;
      (async () => {
        try {
          const { getCurrentWindow } = await import("@tauri-apps/api/window");
          const win = getCurrentWindow();
          await win.setFullscreen(false);
          // Leaving fullscreen drops back to a normal window; restore the
          // maximized state if that's where the user was.
          if (wasMaximized) await win.maximize();
        } catch (err) {
          console.error("[ImmersiveView] exit fullscreen failed", err);
        }
      })();
    };
  }, [prefsLoaded, useNativeFullscreen]);

  const showNowPlaying = dual || classicSide === "nowPlaying";

  return (
    <div
      ref={dialogRef}
      role="dialog"
      aria-modal="true"
      aria-label={t("playerBar.openFullscreen")}
      // Marker so skins exclude the immersive view from their modal /
      // surface chrome overrides — it's a full-screen takeover with its
      // own always-dark styling and must look identical across skins
      // (without this, Liquid repaints it as a light glass modal).
      data-immersive="true"
      // `dark` establishes an always-dark context so the shared player
      // controls (PlaybackControls / VolumeControl / ProgressBar) render
      // their dark-theme variants (hover / track) over the dark backdrop,
      // even when the app theme is light.
      className="dark fixed inset-0 z-100 bg-zinc-950"
    >
      {/* Blurred artwork background — flat dark gradient fallback. Same
          recipe as the old overlays. `animate-fade-in` lives here so the
          opaque `bg-zinc-950` above paints solid from frame 1. */}
      <div className="absolute inset-0 overflow-hidden animate-fade-in">
        {currentTrack?.artwork_path ? (
          <Artwork
            path={currentTrack.artwork_path}
            path1x={currentTrack.artwork_path_1x}
            path2x={currentTrack.artwork_path_2x}
            size="full"
            className="w-full h-full scale-150 blur-3xl"
            alt=""
            rounded="md"
          />
        ) : (
          <div className="w-full h-full bg-linear-to-br from-zinc-800 to-zinc-950" />
        )}
        <div className="absolute inset-0 bg-black/65" />
      </div>

      {/* Foreground */}
      <div className="relative h-full flex flex-col text-white animate-fade-in">
        {/* Shared top bar — panel toggle + share + close. Absolute so
            the columns own the full height underneath. */}
        <div className="absolute top-0 right-0 z-10 flex items-center justify-end gap-3 px-8 py-6">
          {currentTrack &&
            (dual ? (
              <button
                type="button"
                onClick={togglePanel}
                aria-label={t("immersive.togglePanel")}
                aria-pressed={panelOpen}
                title={t("immersive.togglePanel")}
                className={`p-2.5 rounded-full transition-colors ${
                  panelOpen
                    ? "bg-white/25 text-white"
                    : "bg-white/10 hover:bg-white/20 text-white/80"
                }`}
              >
                <PanelRight size={22} />
              </button>
            ) : (
              <button
                type="button"
                onClick={toggleClassicLyrics}
                aria-label={t("playerBar.lyrics")}
                aria-pressed={classicSide === "lyrics"}
                title={t("playerBar.lyrics")}
                className={`p-2.5 rounded-full transition-colors ${
                  classicSide === "lyrics"
                    ? "bg-white/25 text-white"
                    : "bg-white/10 hover:bg-white/20 text-white/80"
                }`}
              >
                <Mic2 size={22} />
              </button>
            ))}
          {/* Show Canvas toggle — only when the current track has a Canvas
              and motion isn't reduced (never a dead control). */}
          {canvasTrack && canvasAvailable && (
            <CanvasToggleButton
              enabled={canvasEnabled}
              onToggle={() => setCanvasEnabled(!canvasEnabled)}
              className={`p-2.5 rounded-full transition-colors ${
                canvasEnabled
                  ? "bg-white/25 text-white"
                  : "bg-white/10 hover:bg-white/20 text-white/80"
              }`}
            />
          )}
          {/* Set / remove the track's Canvas — the immersive "⋯". */}
          {canvasTrack && (
            <button
              type="button"
              onClick={() => {
                setCanvasPickerTarget({
                  trackId: canvasTrack.id,
                  hasCanvas: hasManualCanvas,
                });
                setCanvasPickerOpen(true);
              }}
              aria-label={t("canvas.setTitle")}
              title={t("canvas.setTitle")}
              className="p-2.5 rounded-full bg-white/10 hover:bg-white/20 text-white/80 transition-colors"
            >
              <MoreHorizontal size={22} />
            </button>
          )}
          {currentTrack && <ImmersiveShareButton track={currentTrack} />}
          <button
            type="button"
            onClick={onClose}
            aria-label={t("common.close")}
            className="p-2.5 rounded-full bg-white/10 hover:bg-white/20 transition-colors"
          >
            <X size={22} />
          </button>
        </div>

        {/* Columns */}
        <div className="flex-1 flex min-h-0">
          {showNowPlaying && (
            <div className="min-w-0 flex-1 transition-all duration-300">
              <ImmersiveNowPlaying
                onClose={onClose}
                onNavigateToArtist={onNavigateToArtist}
                isLiked={isLiked}
                onToggleLike={onToggleLike}
              />
            </div>
          )}
          {/* NEW merged view → tabbed control panel on the right. */}
          {dual && panelOpen && currentTrack && (
            <div className="min-w-0 w-2/5 border-l border-white/10">
              <ImmersiveSidePanel
                track={currentTrack}
                lyrics={lyrics}
                activeTab={activeTab}
                onTabChange={setActiveTab}
              />
            </div>
          )}
          {/* CLASSIC view → lyrics-only fullscreen (pre-#328 behaviour). */}
          {!dual && classicSide === "lyrics" && currentTrack && (
            <div className="min-w-0 flex-1">
              <ImmersiveLyricsColumn
                track={currentTrack}
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
                onCoverClick={toggleClassicLyrics}
              />
            </div>
          )}
        </div>
      </div>

      {canvasPickerTarget && (
        <CanvasPickerModal
          trackId={canvasPickerTarget.trackId}
          hasCanvas={canvasPickerTarget.hasCanvas}
          isOpen={canvasPickerOpen}
          onClose={() => setCanvasPickerOpen(false)}
          onSuccess={() => {
            /* invalidateTrackCanvas already ran in the modal; the hooks
               re-resolve on the next render tick. */
          }}
        />
      )}
    </div>
  );
}

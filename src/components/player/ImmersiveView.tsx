import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { X, PanelRight, Mic2, MoreHorizontal, Tv } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import { usePlayer } from "../../hooks/usePlayer";
import { useTrackLyrics } from "../../hooks/useTrackLyrics";
import {
  IMMERSIVE_PREF_KEYS,
  IMMERSIVE_PREFS_EVENT,
  useImmersivePrefs,
} from "../../hooks/useImmersivePrefs";
import { setProfileSetting } from "../../lib/tauri/profile";
import { playerHoldDisplay } from "../../lib/tauri/player";
import { useTrackCanvas } from "../../hooks/useTrackCanvas";
import {
  useCanvasEnabled,
  setCanvasEnabled,
} from "../../hooks/useCanvasEnabled";
import { isRemoteCanvasUrl } from "../../lib/tauri/canvas";
import { usePrefersReducedMotion } from "../../hooks/usePrefersReducedMotion";
import { isRadioTrack, isStreamTrack } from "../../lib/playerSources";
import { PrecomputedBlur } from "../common/PrecomputedBlur";
import { resolveArtwork } from "../../lib/tauri/artwork";
import { ImmersiveNowPlaying } from "./ImmersiveNowPlaying";
import { ImmersiveSidePanel, type ImmersiveTab } from "./ImmersiveSidePanel";
import { ImmersiveLyricsColumn } from "./ImmersiveLyricsColumn";
import { ImmersiveShareButton } from "./ImmersiveShareButton";
import { shareableLyricLines } from "../../lib/lyricsCard";
import { CanvasToggleButton } from "./CanvasToggleButton";
import { CanvasPickerModal } from "../common/CanvasPickerModal";
import { LINUX } from "../../hooks/usePlayableVideo";

/** Below this width the dual-column layout collapses to a single column
 *  with a now-playing ⇄ panel toggle — two columns only make sense at
 *  desktop widths. */
const NARROW_BREAKPOINT = 900;

/**
 * Theater mode lays the view out as if the window were this wide and
 * scales it up to the real width (`zoom`), so a 1080p TV shows it half as
 * large again — readable from the sofa — and a wider one up to twice. A
 * window narrower than this keeps the normal scale.
 */
const THEATER_BASE_WIDTH = 1280;
const THEATER_MAX_ZOOM = 2;
/** Without a pointer move or a key for this long, the theater view hides
 *  its controls and the pointer. */
const THEATER_IDLE_MS = 3000;
/** What wakes the theater view's controls. */
const THEATER_WAKE_EVENTS = [
  "pointermove",
  "pointerdown",
  "keydown",
  "wheel",
] as const;

/**
 * The view fades in everywhere but Linux. WebKitGTK 2.54 rebuilds its
 * layers each time a video appears inside a full-window layer that is
 * still animating, and a motion cover or Canvas mounting during the fade
 * blacked the whole window out three times before it settled. Without the
 * fade one short flash is left: WebKit creating the video layer itself.
 */
const FADE_IN = LINUX ? "" : " animate-fade-in";

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
 * Native fullscreen (`immersive.use_native_fullscreen`, default OFF)
 * drives the OS window into real fullscreen on open and restores the
 * prior window state on close.
 *
 * Theater mode (`immersive.theater_mode`, the Tv button) is the same view
 * set up as a display across the room: always the two columns, the
 * Tracklist tab beside the cover, scaled up to the window, in native
 * fullscreen, the controls and the pointer hidden after a few idle
 * seconds, and the display kept on while the music plays. Escape closes the view; exiting OS
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
  const { currentTrack } = usePlayer();
  const {
    mergedLyrics,
    useNativeFullscreen,
    hideEmptyLyrics,
    theaterMode,
    loaded: prefsLoaded,
  } = useImmersivePrefs();
  // The view's own toggle answers at once; the stored pref catches up.
  const [theaterOverride, setTheaterOverride] = useState<boolean | null>(null);
  const theater = theaterOverride ?? theaterMode;
  const lyrics = useTrackLyrics();

  // Per-track Canvas (issue #442). Only a real library track can carry one
  // (radio uses a negative sentinel id and has no local row), so the
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
  // Two questions, and conflating them hid the server Canvas entirely.
  //
  // `canvasShowable` asks whether this track can DISPLAY one. A server track
  // can: it plays under a negative sentinel, but its clip comes from the
  // server, not from a local row. Requiring a non-negative id here left the
  // "Show Canvas" toggle unrendered — and since the toggle defaults to OFF,
  // that meant the clip could never be revealed at all.
  //
  // `canvasEditable` asks whether the PICKER may act on it, which does need a
  // real library row: the picker writes `track_canvas` keyed on a rowid, and
  // a server clip is not ours to replace or delete in any case.
  const canvasShowable =
    currentTrack && !isRadioTrack(currentTrack) ? currentTrack : null;
  const canvasEditable =
    canvasShowable && canvasShowable.id >= 0 ? canvasShowable : null;
  const canvasAvailable = !!canvasPath && !reducedMotion;
  // The picker sets/removes the MANUAL local Canvas only. A Canvas that came
  // from a plugin (issue #473) or from the bound server is a URL, and must
  // NOT read as "has a Canvas to remove" — neither is ours to delete.
  // `canvasEditable` already keeps a server track out of the picker. Shared
  // local-vs-remote split with CanvasStage.
  const hasManualCanvas = !!canvasPath && !isRemoteCanvasUrl(canvasPath);

  // Escape close + focus trap. Only mounted while open → pass `true`.
  const dialogRef = useModalA11y<HTMLDivElement>(true, onClose);

  // Track the viewport width so the dual layout collapses on narrow
  // windows. `window.innerWidth` is fine — the view is always
  // full-window so there's no inner container to measure.
  const [viewportWidth, setViewportWidth] = useState(() => window.innerWidth);
  useEffect(() => {
    const onResize = () => setViewportWidth(window.innerWidth);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);
  const narrow = viewportWidth < NARROW_BREAKPOINT;
  const theaterZoom = theater
    ? Math.min(
        THEATER_MAX_ZOOM,
        Math.max(1, viewportWidth / THEATER_BASE_WIDTH),
      )
    : 1;

  // Two genuinely distinct interfaces, chosen by the merged pref (and
  // forced to the classic one on narrow windows where two columns don't
  // fit):
  //  - `dual` → NEW merged view: now-playing + a tabbed control panel
  //    (Lyrics / Queue) on the right, toggled with a PanelRight button.
  //  - else  → CLASSIC view: now-playing fullscreen with a Mic2 button
  //    that flips to a lyrics-only fullscreen (the pre-#328 behaviour,
  //    unchanged — no panel, no queue).
  // Theater mode always has the two columns: the tracklist beside the
  // cover is what it is for.
  const dual = (mergedLyrics || theater) && !narrow;

  // Static source for the blurred backdrop: a pre-resized variant or
  // nothing. Never a library track's `artwork_path`, which can be the
  // animated file the cover picker accepts — see the background comment
  // below (#615). A stream (radio, remote queue) has no variants and no
  // cover picker: its `artwork_path` is a cover fetched for the song or
  // the station's logo, both still images, and without it a radio played
  // on plain black while the mini-player took its colours (#763).
  const streamBackdrop = isStreamTrack(currentTrack)
    ? (currentTrack?.artwork_path ?? null)
    : null;
  const staticBackdrop =
    currentTrack?.artwork_path_2x ??
    currentTrack?.artwork_path_1x ??
    streamBackdrop;

  // Dual: which tab the control panel shows + whether it's open.
  const [activeTab, setActiveTab] = useState<ImmersiveTab>("lyrics");
  const [panelOpen, setPanelOpen] = useState(true);
  // Classic: which of the two fullscreens is visible. Opening via the
  // lyrics button lands on lyrics.
  const [classicSide, setClassicSide] = useState<"nowPlaying" | "lyrics">(
    initialTab === "lyrics" ? "lyrics" : "nowPlaying",
  );

  // A track with no lyrics can close the lyrics column on its own
  // (#794, opt-in). Only a finished lookup decides: while the next one
  // runs, the column keeps the previous verdict, so two tracks without
  // lyrics in a row do not flash it open in between. A failed lookup is
  // not "no lyrics" — the column carries its retry.
  const lyricsAreEmpty =
    (lyrics.payload == null ||
      lyrics.payload.content.trim() === "" ||
      lyrics.excludedGenre != null) &&
    lyrics.error == null;
  const [settledEmpty, setSettledEmpty] = useState(false);
  if (!lyrics.isFetching && settledEmpty !== lyricsAreEmpty) {
    setSettledEmpty(lyricsAreEmpty);
  }
  // Reopening the column by hand wins until the song changes. A radio
  // station keeps one id from song to song, so the title and artist are
  // part of what "this song" means.
  const songKey = currentTrack
    ? `${currentTrack.id}:${currentTrack.artist_name ?? ""}:${currentTrack.title}`
    : null;
  const [shownAnywayFor, setShownAnywayFor] = useState<string | null>(null);
  // Forgotten as soon as the song changes, so coming back to it later
  // starts from the setting again.
  const [lastSongKey, setLastSongKey] = useState(songKey);
  if (lastSongKey !== songKey) {
    setLastSongKey(songKey);
    setShownAnywayFor(null);
  }
  const autoHidden =
    hideEmptyLyrics &&
    settledEmpty &&
    activeTab === "lyrics" &&
    shownAnywayFor !== songKey;
  const panelShown = panelOpen && !autoHidden;

  const togglePanel = () => {
    // Hidden for want of lyrics, whatever the user's own toggle says: one
    // press shows it, not two.
    if (autoHidden) {
      setShownAnywayFor(songKey);
      setPanelOpen(true);
      return;
    }
    setPanelOpen((v) => !v);
  };
  const toggleClassicLyrics = () =>
    setClassicSide((s) => (s === "lyrics" ? "nowPlaying" : "lyrics"));

  // ── Theater mode ─────────────────────────────────────────────────
  const toggleTheater = () => {
    const next = !theater;
    setTheaterOverride(next);
    setProfileSetting(
      IMMERSIVE_PREF_KEYS.theaterMode,
      next ? "true" : "false",
      "bool",
    )
      .then(() => window.dispatchEvent(new CustomEvent(IMMERSIVE_PREFS_EVENT)))
      .catch((err) =>
        console.error("[ImmersiveView] save theater mode failed", err),
      );
  };

  // Entering theater mode opens the panel on the tracklist — unless the
  // view was opened for its lyrics and is only starting in theater mode.
  const [theaterShown, setTheaterShown] = useState(false);
  if (theater !== theaterShown) {
    setTheaterShown(theater);
    if (theater) {
      setPanelOpen(true);
      if (theaterOverride !== null || initialTab !== "lyrics") {
        setActiveTab("tracklist");
      }
    }
  }

  // Idle: no pointer move and no key for a few seconds hides the controls
  // and the pointer (CSS on `data-idle`); any of them brings them back. A
  // control the keyboard had focused is let go, so the key that wakes the
  // view reaches the shortcuts rather than pressing an unseen button.
  const [idle, setIdle] = useState(false);
  useEffect(() => {
    if (!theater) return;
    let timer = 0;
    const arm = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        setIdle(true);
        const focused = document.activeElement;
        if (
          focused instanceof HTMLElement &&
          focused.closest("[data-theater-chrome]")
        ) {
          focused.blur();
        }
      }, THEATER_IDLE_MS);
    };
    const wake = () => {
      setIdle(false);
      arm();
    };
    arm();
    for (const type of THEATER_WAKE_EVENTS) {
      window.addEventListener(type, wake, { capture: true, passive: true });
    }
    return () => {
      window.clearTimeout(timer);
      for (const type of THEATER_WAKE_EVENTS) {
        window.removeEventListener(type, wake, { capture: true });
      }
    };
  }, [theater]);
  const chromeHidden = theater && idle;

  // The display stays on while the music plays, for as long as theater
  // mode lasts (`power.rs`); given back when it ends or the view closes.
  useEffect(() => {
    if (!theater) return;
    playerHoldDisplay(true).catch((err) =>
      console.error("[ImmersiveView] hold display failed", err),
    );
    return () => {
      playerHoldDisplay(false).catch((err) =>
        console.error("[ImmersiveView] release display failed", err),
      );
    };
  }, [theater]);

  // ── Native fullscreen (reversible) ───────────────────────────────
  // The floating top-right buttons, which the side panel measures.
  const buttonBarRef = useRef<HTMLDivElement>(null);

  // Enter on mount, restore the prior window state on unmount. Refs so
  // the cleanup restores exactly what we changed even if mount/unmount
  // race a very fast open→close.
  //
  // Theater mode asks for it too, whatever the pref: turning it off leaves
  // fullscreen again, unless the pref wants it for the whole view.
  const wantNativeFullscreen = useNativeFullscreen || theater;
  const enteredFullscreenRef = useRef(false);
  const priorMaximizedRef = useRef(false);
  useEffect(() => {
    // Wait for the real pref before touching the OS window — acting on
    // the optimistic default would flash a user who disabled native
    // fullscreen into fullscreen for one tick.
    if (!prefsLoaded || !wantNativeFullscreen) return;
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
  }, [prefsLoaded, wantNativeFullscreen]);

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
      // Theater mode, and whether its controls are hidden for want of
      // input (`app.css`).
      data-theater={theater || undefined}
      data-idle={chromeHidden || undefined}
    >
      {/* Blurred artwork background — flat dark gradient fallback. Same
          recipe as the old overlays. The fade-in (`FADE_IN`, none on
          Linux) lives here so the opaque `bg-zinc-950` above paints solid
          from frame 1.

          Deliberately a pre-resized variant, and **only** one: behind a
          64 px blur at 150% scale a 128 px source is indistinguishable,
          and the thumbnails the artwork pipeline generates are
          single-frame. An animated cover — what "Change cover → Local
          file" accepts, since it takes webp and refuses video — was
          otherwise decoded frame by frame *and* blurred over the whole
          screen, which is the stutter reported in #615.

          Hence the branch on `staticBackdrop` rather than on
          `artwork_path`, and no `path` passed for a library track:
          `size="2x"` falls back to the full source when neither thumbnail
          exists, which would have put the animated file right back here on
          an album the thumbnail worker has not caught up with. No static
          variant means the gradient, not the animation. It still plays,
          once, on the foreground cover. A stream's `path` is the one
          exception — see `streamBackdrop`.

          Blurred once, not once per frame (`PrecomputedBlur`): a CSS blur
          over the whole screen is recomputed under every frame the lyrics
          or the visualizer repaint, which held the view to 4–9 frames a
          second under WebKitGTK's software renderer. */}
      <div className={`absolute inset-0 overflow-hidden${FADE_IN}`}>
        {staticBackdrop ? (
          <PrecomputedBlur
            key={staticBackdrop}
            src={resolveArtwork(
              {
                full: streamBackdrop,
                x1: currentTrack?.artwork_path_1x ?? null,
                x2: currentTrack?.artwork_path_2x ?? null,
              },
              "2x",
            )}
            blur={64}
            scale={1.5}
          />
        ) : (
          <div className="w-full h-full bg-linear-to-br from-zinc-800 to-zinc-950" />
        )}
        <div className="absolute inset-0 bg-black/65" />
      </div>

      {/* Foreground */}
      <div
        className={`relative h-full flex flex-col text-white${FADE_IN}`}
        style={theaterZoom !== 1 ? { zoom: theaterZoom } : undefined}
      >
        {/* Shared top bar — panel toggle + share + close. Absolute so
            the columns own the full height underneath. The side panel
            measures it to know how much of its own top line is free.

            Its padding is click-through: only the buttons take the
            pointer. The bar sits above the columns (`z-10`), so its
            transparent 32px left padding and 24px above and below the
            buttons swallowed clicks meant for what is underneath — the
            end of the side panel's tabs when they fit with little to
            spare (#752 review), and a strip of the lyrics or queue. */}
        <div
          ref={buttonBarRef}
          data-theater-chrome
          className="absolute top-0 right-0 z-10 flex items-center justify-end gap-3 px-8 py-6 pointer-events-none *:pointer-events-auto"
        >
          {currentTrack &&
            (dual ? (
              <button
                type="button"
                onClick={togglePanel}
                aria-label={t("immersive.togglePanel")}
                aria-pressed={panelShown}
                title={t("immersive.togglePanel")}
                className={`p-2.5 rounded-full transition-colors ${
                  panelShown
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
          {canvasShowable && canvasAvailable && (
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
          {canvasEditable && (
            <button
              type="button"
              onClick={() => {
                setCanvasPickerTarget({
                  trackId: canvasEditable.id,
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
          {currentTrack && (
            <ImmersiveShareButton
              track={currentTrack}
              // Empty while a fetch runs: the previous track's lyrics
              // linger until the new ones land.
              lyricLines={lyrics.isFetching ? [] : shareableLyricLines(lyrics)}
              activeLyricIndex={lyrics.activeIndex}
            />
          )}
          <button
            type="button"
            onClick={toggleTheater}
            aria-label={t("immersive.theater")}
            aria-pressed={theater}
            title={t("immersive.theater")}
            className={`p-2.5 rounded-full transition-colors ${
              theater
                ? "bg-white/25 text-white"
                : "bg-white/10 hover:bg-white/20 text-white/80"
            }`}
          >
            <Tv size={22} />
          </button>
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
          {dual && panelShown && currentTrack && (
            <div className="min-w-0 w-2/5 border-l border-white/10">
              <ImmersiveSidePanel
                track={currentTrack}
                lyrics={lyrics}
                activeTab={activeTab}
                onTabChange={setActiveTab}
                buttonBarRef={buttonBarRef}
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
                activeBackgroundWordIndex={lyrics.activeBackgroundWordIndex}
                overlapIndex={lyrics.overlapIndex}
                overlapWordIndex={lyrics.overlapWordIndex}
                overlapBackgroundWordIndex={lyrics.overlapBackgroundWordIndex}
                activeInterlude={lyrics.activeInterlude}
                isFetching={lyrics.isFetching}
                error={lyrics.error}
                excludedGenre={lyrics.excludedGenre}
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

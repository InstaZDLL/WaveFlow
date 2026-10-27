import { type Playlist } from "../../../lib/tauri/playlist";
import { type Translator } from "./libraryTypes";
import { useLayoutEffect, useRef, useState } from "react";
import { resolvePlaylistColor } from "../../../lib/playlistVisuals";
import { PlaylistIcon } from "../../../lib/PlaylistIcon";
import { Check, Plus } from "lucide-react";
import { createPortal } from "react-dom";

interface AddToPlaylistPopoverProps {
  playlists: Playlist[];
  trackId: number;
  onPick: (playlistId: number) => void;
  onCreate: () => void;
  t: Translator;
  /**
   * Optional set of playlist IDs the target is already in. Only meaningful
   * for the track popover — when provided, matching rows render a green
   * checkmark and the caller is expected to toggle (remove) rather than
   * add on click. Albums/artists/folders skip this prop because their
   * "+ to playlist" action is a bulk add with no symmetric remove.
   */
  memberPlaylistIds?: ReadonlySet<number>;
  /**
   * Trigger element the popover anchors to. When provided, the popover
   * is rendered through a portal at `document.body` and positioned via
   * `getBoundingClientRect`, escaping every ancestor stacking context
   * (virtualizer rows use `transform`, which traps `z-index` inside).
   * Required for album / artist grids where the popover would otherwise
   * paint under the row below it.
   */
  anchorEl?: HTMLElement | null;
}

/**
 * Tiny popover anchored to the trigger button. Lists every playlist of
 * the active profile (resolved color tile + name) plus a "create new"
 * shortcut at the bottom. Picking a row calls `onPick(playlistId)`.
 *
 * When `anchorEl` is supplied, the popover is rendered via React portal
 * to `document.body` and positioned absolutely against the anchor's
 * client rect. Without it, the popover falls back to absolute positioning
 * inside its parent — only safe where the parent isn't sitting inside a
 * `transform`-clipped stacking context (TrackTable rows qualify; album /
 * artist grids don't).
 *
 * Stops `onDoubleClick` from bubbling to the parent so clicking a
 * playlist doesn't accidentally start playback of the row underneath.
 */
export function AddToPlaylistPopover({
  playlists: allPlaylists,
  onPick,
  onCreate,
  t,
  memberPlaylistIds,
  anchorEl,
}: AddToPlaylistPopoverProps) {
  // Only playlists a track can be added to: a smart playlist is rebuilt
  // from its rules and would drop the track at the next regeneration.
  const playlists = allPlaylists.filter((pl) => pl.is_smart !== 1);
  // Portal mode: track the anchor's viewport rect AND the popover's own
  // height so we can flip / clamp against the viewport. `null` rect =
  // first render before the layout effect runs; we keep the popover
  // invisible until we know where it goes so it never flashes at (0,0).
  const POPOVER_WIDTH = 224; // matches `w-56`
  const VIEWPORT_MARGIN = 8;
  const popoverRef = useRef<HTMLDivElement | null>(null);
  const [rect, setRect] = useState<DOMRect | null>(null);
  const [popoverHeight, setPopoverHeight] = useState(0);
  useLayoutEffect(() => {
    if (!anchorEl) return;
    const update = () => setRect(anchorEl.getBoundingClientRect());
    update();
    const ro = new ResizeObserver(update);
    ro.observe(anchorEl);
    window.addEventListener("scroll", update, true);
    window.addEventListener("resize", update);
    return () => {
      ro.disconnect();
      window.removeEventListener("scroll", update, true);
      window.removeEventListener("resize", update);
    };
  }, [anchorEl]);
  // Measure the popover the first time it lays out and on content
  // resize so the flip-above check has a real height. We intentionally
  // do NOT depend on `rect` — scroll updates `rect` many times per
  // second, and re-running this effect would tear down the
  // ResizeObserver and force a synchronous `offsetHeight` reflow each
  // tick. The ResizeObserver already covers every real height change
  // (translated label wrap, scrollable list growth, etc.).
  useLayoutEffect(() => {
    if (!anchorEl) return;
    const el = popoverRef.current;
    if (!el) return;
    setPopoverHeight(el.offsetHeight);
    const ro = new ResizeObserver(() => setPopoverHeight(el.offsetHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, [anchorEl]);

  // Compute placement: prefer below, flip above when below would clip,
  // then clamp horizontally so the first-column trigger doesn't push
  // the popover off the left edge.
  const placement = (() => {
    if (!anchorEl || !rect) return null;
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    let top = rect.bottom + 4;
    if (
      popoverHeight > 0 &&
      top + popoverHeight > vh - VIEWPORT_MARGIN &&
      rect.top - 4 - popoverHeight >= VIEWPORT_MARGIN
    ) {
      top = rect.top - 4 - popoverHeight;
    }
    top = Math.max(
      VIEWPORT_MARGIN,
      Math.min(top, vh - popoverHeight - VIEWPORT_MARGIN),
    );
    let left = rect.right - POPOVER_WIDTH;
    left = Math.max(
      VIEWPORT_MARGIN,
      Math.min(left, vw - POPOVER_WIDTH - VIEWPORT_MARGIN),
    );
    return { top, left };
  })();

  const inner = (
    <div
      ref={popoverRef}
      data-add-to-playlist-popover
      role="menu"
      // Stop click + double-click + mousedown from bubbling to the
      // album / artist tile underneath. Portals re-parent the DOM but
      // React events still bubble through the React tree, so without
      // this picking a playlist would also navigate to the album.
      onClick={(e) => e.stopPropagation()}
      onMouseDown={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      style={
        anchorEl
          ? placement
            ? {
                position: "fixed",
                top: placement.top,
                left: placement.left,
                width: POPOVER_WIDTH,
              }
            : { position: "fixed", visibility: "hidden" }
          : undefined
      }
      className={`${
        anchorEl ? "z-100" : "absolute top-full right-0 mt-1 z-50 w-56"
      } rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated dark:shadow-black/40 overflow-hidden animate-fade-in`}
    >
      <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase px-3 pt-3 pb-2">
        {t("trackActions.addToPlaylist")}
      </div>
      <div className="px-2 max-h-64 overflow-y-auto">
        {playlists.length === 0 ? (
          <div className="px-2 py-3 text-xs text-zinc-400 text-center">
            {t("trackActions.noPlaylists")}
          </div>
        ) : (
          playlists.map((pl) => {
            const color = resolvePlaylistColor(pl.color_id);
            const isMember = memberPlaylistIds?.has(pl.id) ?? false;
            return (
              <button
                key={pl.id}
                type="button"
                role="menuitem"
                aria-label={
                  isMember
                    ? t("trackActions.removeFromPlaylistNamed", {
                        name: pl.name,
                      })
                    : t("trackActions.addToPlaylistNamed", { name: pl.name })
                }
                onClick={() => onPick(pl.id)}
                className="w-full flex items-center space-x-2 p-2 rounded-lg text-left hover:bg-zinc-50 dark:hover:bg-zinc-700/30 transition-colors"
              >
                <div
                  className={`w-7 h-7 rounded-md flex items-center justify-center shrink-0 ${color.tileBg} ${color.tileText}`}
                >
                  <PlaylistIcon iconId={pl.icon_id} size={14} />
                </div>
                <span className="flex-1 text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
                  {pl.name}
                </span>
                {isMember && (
                  <Check
                    size={14}
                    className="shrink-0 text-emerald-500"
                    aria-hidden="true"
                  />
                )}
              </button>
            );
          })
        )}
      </div>
      <div className="border-t border-zinc-100 dark:border-zinc-700/50">
        <button
          type="button"
          role="menuitem"
          onClick={onCreate}
          className="w-full flex items-center space-x-2 px-3 py-2 text-left text-sm font-medium text-emerald-500 hover:bg-emerald-50 dark:hover:bg-emerald-900/20 transition-colors"
        >
          <Plus size={14} />
          <span>{t("trackActions.createPlaylist")}</span>
        </button>
      </div>
    </div>
  );
  return anchorEl ? createPortal(inner, document.body) : inner;
}

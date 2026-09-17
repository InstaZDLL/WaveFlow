import { useTranslation } from "react-i18next";
import { Lock, MessageSquareQuote, RotateCcw } from "lucide-react";

import { ToggleSwitch } from "../../common/ToggleSwitch";
import { useDesktopLyricsStatus } from "../../../hooks/useDesktopLyricsStatus";
import {
  BACKGROUND_OPACITY_MAX,
  FONT_SIZE_MAX,
  FONT_SIZE_MIN,
  useDesktopLyricsStyle,
} from "../../../hooks/useDesktopLyricsStyle";

/**
 * Settings → Appearance → Desktop lyrics (issue #582): open the floating
 * window, lock it, and choose how its text looks.
 *
 * The lock lives here as well as in the tray and the player bar's "⋯"
 * menu for one reason: a locked window ignores the mouse, so every place
 * that can lock it must be a place that can unlock it.
 */
export function DesktopLyricsCard() {
  const { t } = useTranslation();
  const { status, toggleOpen, setLocked } = useDesktopLyricsStatus();
  const { style, ready, update, reset } = useDesktopLyricsStyle();

  return (
    <section
      aria-label={t("desktopLyrics.title")}
      className="px-4 py-3 space-y-4"
    >
      <div className="flex items-start justify-between gap-3">
        <span className="flex items-start gap-3 min-w-0">
          <MessageSquareQuote
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("desktopLyrics.title")}
            </span>
            <span className="block text-xs mt-0.5 settings-description">
              {t("settings.desktopLyrics.subtitle")}
            </span>
          </span>
        </span>
        <ToggleSwitch
          enabled={status.open}
          onToggle={toggleOpen}
          label={t("desktopLyrics.title")}
        />
      </div>

      <div className="flex items-start justify-between gap-3">
        <span className="flex items-start gap-3 min-w-0">
          <Lock
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("desktopLyrics.lock")}
            </span>
            <span className="block text-xs mt-0.5 settings-description">
              {t("desktopLyrics.lockHint")}
            </span>
          </span>
        </span>
        <ToggleSwitch
          enabled={status.locked}
          onToggle={() => setLocked(!status.locked)}
          label={t("desktopLyrics.lock")}
          disabled={!status.open}
        />
      </div>

      {status.wayland && (
        <p className="text-xs settings-description">
          {t("settings.desktopLyrics.waylandNote")}
        </p>
      )}

      {/* Preview on a fixed dark ground: the overlay floats over whatever
          is on the desktop, and this is closer to that than the settings
          page's own background would be. */}
      <div
        aria-hidden="true"
        className="rounded-xl bg-zinc-800 bg-[linear-gradient(135deg,#3f3f46,#18181b)] px-4 py-5 text-center overflow-hidden"
      >
        <div
          className="inline-block max-w-full rounded-lg px-3 py-2"
          style={{
            backgroundColor: `rgba(0,0,0,${style.backgroundOpacity / 100})`,
          }}
        >
          <p
            className="truncate font-bold leading-tight"
            style={{
              fontSize: Math.min(style.fontSize, 40),
              textShadow: style.outline
                ? "-1px -1px 0 rgba(0,0,0,.85), 1px -1px 0 rgba(0,0,0,.85), -1px 1px 0 rgba(0,0,0,.85), 1px 1px 0 rgba(0,0,0,.85)"
                : undefined,
            }}
          >
            <span style={{ color: style.highlightColor }}>
              {t("settings.desktopLyrics.previewSung")}
            </span>{" "}
            <span style={{ color: style.textColor }}>
              {t("settings.desktopLyrics.previewUnsung")}
            </span>
          </p>
        </div>
      </div>

      <fieldset disabled={!ready} className="grid gap-3 @xl:grid-cols-2">
        <legend className="sr-only">
          {t("settings.organization.groups.desktopLyrics")}
        </legend>
        <label className="flex flex-col gap-1 text-sm text-zinc-700 dark:text-zinc-200">
          <span className="flex justify-between">
            {t("settings.desktopLyrics.fontSize")}
            <span className="tabular-nums text-zinc-500">
              {style.fontSize}px
            </span>
          </span>
          <input
            type="range"
            min={FONT_SIZE_MIN}
            max={FONT_SIZE_MAX}
            step={2}
            value={style.fontSize}
            onChange={(e) => update({ fontSize: Number(e.target.value) })}
            className="accent-emerald-500"
          />
        </label>

        <label className="flex flex-col gap-1 text-sm text-zinc-700 dark:text-zinc-200">
          <span className="flex justify-between">
            {t("settings.desktopLyrics.background")}
            <span className="tabular-nums text-zinc-500">
              {style.backgroundOpacity}%
            </span>
          </span>
          <input
            type="range"
            min={0}
            max={BACKGROUND_OPACITY_MAX}
            step={5}
            value={style.backgroundOpacity}
            onChange={(e) =>
              update({ backgroundOpacity: Number(e.target.value) })
            }
            className="accent-emerald-500"
          />
        </label>

        <label className="flex items-center justify-between gap-3 text-sm text-zinc-700 dark:text-zinc-200">
          {t("settings.desktopLyrics.textColor")}
          <input
            type="color"
            value={style.textColor}
            onChange={(e) => update({ textColor: e.target.value })}
            className="h-8 w-12 cursor-pointer rounded border border-zinc-200 dark:border-zinc-700 bg-transparent"
          />
        </label>

        <label className="flex items-center justify-between gap-3 text-sm text-zinc-700 dark:text-zinc-200">
          {t("settings.desktopLyrics.highlightColor")}
          <input
            type="color"
            value={style.highlightColor}
            onChange={(e) => update({ highlightColor: e.target.value })}
            className="h-8 w-12 cursor-pointer rounded border border-zinc-200 dark:border-zinc-700 bg-transparent"
          />
        </label>

        <label className="flex items-center justify-between gap-3 text-sm text-zinc-700 dark:text-zinc-200 cursor-pointer">
          {t("settings.desktopLyrics.outline")}
          <input
            type="checkbox"
            checked={style.outline}
            onChange={(e) => update({ outline: e.target.checked })}
            className="w-4 h-4 accent-emerald-500 cursor-pointer"
          />
        </label>

        <label className="flex items-center justify-between gap-3 text-sm text-zinc-700 dark:text-zinc-200 cursor-pointer">
          {t("settings.desktopLyrics.showTranslation")}
          <input
            type="checkbox"
            checked={style.showTranslation}
            onChange={(e) => update({ showTranslation: e.target.checked })}
            className="w-4 h-4 accent-emerald-500 cursor-pointer"
          />
        </label>
      </fieldset>

      <button
        type="button"
        onClick={reset}
        disabled={!ready}
        className="inline-flex items-center gap-1.5 text-xs font-medium text-zinc-500 hover:text-zinc-800 dark:hover:text-white disabled:opacity-50"
      >
        <RotateCcw size={12} aria-hidden="true" />
        {t("settings.desktopLyrics.reset")}
      </button>
    </section>
  );
}

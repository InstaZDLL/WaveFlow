import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Activity,
  Maximize2,
  Menu,
  Mic2,
  MonitorSpeaker,
  Moon,
  PictureInPicture2,
  Repeat2,
  SlidersHorizontal,
  type LucideIcon,
} from "lucide-react";
import {
  COVER_ACTION_KEY,
  PLAYER_BAR_LAYOUT_EVENT,
  PLAYER_BAR_LAYOUT_KEYS,
  usePlayerBarLayout,
  type CoverAction,
  type PlayerBarLayout,
} from "../../../hooks/usePlayerBarLayout";
import { setProfileSetting } from "../../../lib/tauri/profile";

/**
 * Unified Settings → Playback section that replaces the per-feature
 * pin toggles (sleep timer, A-B loop, audio quality footer) with a
 * single panel covering every player-bar button + the cover-click
 * action selector. Order is fixed (matches `PlayerBar.tsx` render
 * order); user input is restricted to show/hide toggles and a single
 * radio choice for the cover action.
 *
 * Why a fixed order instead of drag-reorder: the player bar is small
 * enough that the spatial mapping is conventional (Spotify / Apple
 * Music both ship a fixed sequence). Drag adds noise and a sortable
 * library dependency for no measurable user gain.
 *
 * Writes flow through [`setProfileSetting`] then a single
 * `waveflow:playerbar-layout-changed` window event so the player
 * bar's `usePlayerBarLayout` hook re-reads in one go (no per-key
 * event proliferation).
 */
export function PlayerBarLayoutCard() {
  const { t } = useTranslation();
  const layout = usePlayerBarLayout();
  // Track whether any write is in-flight so we can dim the panel
  // briefly. Optimistic state lives in the hook (event-driven), not
  // here, so the visible toggles update before the round-trip
  // resolves.
  const [busyKey, setBusyKey] = useState<string | null>(null);

  const writeBool = useCallback(async (key: string, value: boolean) => {
    setBusyKey(key);
    try {
      await setProfileSetting(key, value ? "true" : "false", "bool");
      window.dispatchEvent(new CustomEvent(PLAYER_BAR_LAYOUT_EVENT));
    } catch (err) {
      console.error(`[PlayerBarLayoutCard] set ${key} failed`, err);
    } finally {
      setBusyKey(null);
    }
  }, []);

  const writeCoverAction = useCallback(async (value: CoverAction) => {
    setBusyKey(COVER_ACTION_KEY);
    try {
      await setProfileSetting(COVER_ACTION_KEY, value, "string");
      window.dispatchEvent(new CustomEvent(PLAYER_BAR_LAYOUT_EVENT));
    } catch (err) {
      console.error("[PlayerBarLayoutCard] set cover action failed", err);
    } finally {
      setBusyKey(null);
    }
  }, []);

  // Button definitions — kept in render order so the preview grid
  // mirrors the actual bar layout left-to-right.
  const buttonRows: Array<{
    prop: Exclude<keyof PlayerBarLayout, "coverAction">;
    settingKey: string;
    icon: LucideIcon;
    labelKey: string;
  }> = [
    {
      prop: "showAbLoop",
      settingKey: PLAYER_BAR_LAYOUT_KEYS.showAbLoop,
      icon: Repeat2,
      labelKey: "settings.playerBarLayout.buttons.abLoop",
    },
    {
      prop: "showSleepTimer",
      settingKey: PLAYER_BAR_LAYOUT_KEYS.showSleepTimer,
      icon: Moon,
      labelKey: "settings.playerBarLayout.buttons.sleepTimer",
    },
    {
      prop: "showEqPreset",
      settingKey: PLAYER_BAR_LAYOUT_KEYS.showEqPreset,
      icon: SlidersHorizontal,
      labelKey: "settings.playerBarLayout.buttons.eqPreset",
    },
    {
      prop: "showLyrics",
      settingKey: PLAYER_BAR_LAYOUT_KEYS.showLyrics,
      icon: Mic2,
      labelKey: "settings.playerBarLayout.buttons.lyrics",
    },
    {
      prop: "showQueue",
      settingKey: PLAYER_BAR_LAYOUT_KEYS.showQueue,
      icon: Menu,
      labelKey: "settings.playerBarLayout.buttons.queue",
    },
    {
      prop: "showDevice",
      settingKey: PLAYER_BAR_LAYOUT_KEYS.showDevice,
      icon: MonitorSpeaker,
      labelKey: "settings.playerBarLayout.buttons.device",
    },
    {
      prop: "showMiniPlayer",
      settingKey: PLAYER_BAR_LAYOUT_KEYS.showMiniPlayer,
      icon: PictureInPicture2,
      labelKey: "settings.playerBarLayout.buttons.miniPlayer",
    },
    {
      prop: "showImmersive",
      settingKey: PLAYER_BAR_LAYOUT_KEYS.showImmersive,
      icon: Maximize2,
      labelKey: "settings.playerBarLayout.buttons.immersive",
    },
    {
      prop: "showAudioQualityFooter",
      settingKey: PLAYER_BAR_LAYOUT_KEYS.showAudioQualityFooter,
      icon: Activity,
      labelKey: "settings.playerBarLayout.buttons.audioQualityFooter",
    },
  ];

  const coverOptions: Array<{
    value: CoverAction;
    labelKey: string;
    descriptionKey: string;
  }> = [
    {
      value: "immersive",
      labelKey: "settings.playerBarLayout.coverAction.immersive.label",
      descriptionKey: "settings.playerBarLayout.coverAction.immersive.description",
    },
    {
      value: "now_playing",
      labelKey: "settings.playerBarLayout.coverAction.nowPlaying.label",
      descriptionKey:
        "settings.playerBarLayout.coverAction.nowPlaying.description",
    },
    {
      value: "none",
      labelKey: "settings.playerBarLayout.coverAction.none.label",
      descriptionKey: "settings.playerBarLayout.coverAction.none.description",
    },
  ];

  const visibleButtons = buttonRows.filter((row) => layout[row.prop]);

  return (
    <section
      aria-label={t("settings.playerBarLayout.title")}
      className="space-y-4"
    >
      <header className="px-4">
        <h3 className="text-sm font-semibold text-zinc-900 dark:text-white">
          {t("settings.playerBarLayout.title")}
        </h3>
        <p className="mt-1 text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed">
          {t("settings.playerBarLayout.subtitle")}
        </p>
      </header>

      {/* Preview — icon grid showing every currently-visible button
        in render order. Updates live as the user toggles checkboxes
        below. The `audioQualityFooter` lives BELOW the player bar
        rather than inside the right cluster, so it's intentionally
        excluded from this preview row — the toggle is still in the
        grid below. */}
      <div className="mx-4 rounded-xl border border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800/40 px-4 py-3">
        <div className="text-[10px] font-bold tracking-widest uppercase text-zinc-400 mb-2">
          {t("settings.playerBarLayout.preview")}
        </div>
        {visibleButtons.length === 0 ? (
          <p className="text-xs text-zinc-500 italic py-2">
            {t("settings.playerBarLayout.previewEmpty")}
          </p>
        ) : (
          <div className="flex flex-wrap items-center gap-2">
            {visibleButtons
              .filter((b) => b.prop !== "showAudioQualityFooter")
              .map(({ prop, icon: Icon, labelKey }) => (
                <div
                  key={prop}
                  title={t(labelKey)}
                  aria-label={t(labelKey)}
                  className="p-2 rounded-lg bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-700 text-zinc-500 dark:text-zinc-400"
                >
                  <Icon size={18} aria-hidden="true" />
                </div>
              ))}
          </div>
        )}
      </div>

      {/* Toggle grid */}
      <ul className="mx-4 grid grid-cols-1 sm:grid-cols-2 gap-1">
        {buttonRows.map(({ prop, settingKey, icon: Icon, labelKey }) => {
          const checked = layout[prop];
          const isBusy = busyKey === settingKey;
          return (
            <li key={prop}>
              <label
                className={`flex items-center justify-between gap-3 px-3 py-2.5 rounded-lg cursor-pointer hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors ${
                  isBusy ? "opacity-60" : ""
                }`}
              >
                <span className="flex items-center gap-3 min-w-0">
                  <Icon
                    size={18}
                    className="text-zinc-400 shrink-0"
                    aria-hidden="true"
                  />
                  <span className="text-sm text-zinc-800 dark:text-zinc-200 truncate">
                    {t(labelKey)}
                  </span>
                </span>
                <input
                  type="checkbox"
                  checked={checked}
                  disabled={isBusy}
                  onChange={(e) => writeBool(settingKey, e.target.checked)}
                  className="w-4 h-4 accent-emerald-500 cursor-pointer disabled:cursor-not-allowed"
                />
              </label>
            </li>
          );
        })}
      </ul>

      {/* Cover action — radio group */}
      <fieldset className="mx-4 pt-4 border-t border-zinc-100 dark:border-zinc-800">
        <legend className="text-xs font-medium text-zinc-700 dark:text-zinc-200 mb-2">
          {t("settings.playerBarLayout.coverAction.title")}
        </legend>
        <div className="space-y-1">
          {coverOptions.map(({ value, labelKey, descriptionKey }) => {
            const checked = layout.coverAction === value;
            const isBusy = busyKey === COVER_ACTION_KEY;
            return (
              <label
                key={value}
                className={`flex items-start gap-3 px-3 py-2 rounded-lg cursor-pointer hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors ${
                  isBusy ? "opacity-60" : ""
                }`}
              >
                <input
                  type="radio"
                  name="cover-action"
                  value={value}
                  checked={checked}
                  disabled={isBusy}
                  onChange={() => writeCoverAction(value)}
                  className="mt-0.5 w-4 h-4 accent-emerald-500 cursor-pointer disabled:cursor-not-allowed"
                />
                <span className="min-w-0">
                  <span className="block text-sm text-zinc-800 dark:text-zinc-200">
                    {t(labelKey)}
                  </span>
                  <span className="block text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed">
                    {t(descriptionKey)}
                  </span>
                </span>
              </label>
            );
          })}
        </div>
      </fieldset>
    </section>
  );
}

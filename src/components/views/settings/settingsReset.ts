import {
  createContext,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { listen } from "@tauri-apps/api/event";

import {
  SETTINGS,
  SETTING_KEYS,
  type SettingKey,
} from "../../../lib/settingsRegistry";
import { SETTING_WRITTEN_EVENT } from "../../../lib/settingWrites";
import type { SettingsCategory } from "./settingsCatalog";
import { useProfile } from "../../../hooks/useProfile";
import { useSkin } from "../../../hooks/useSkin";
import { useTheme } from "../../../hooks/useTheme";
import { parseBoolean } from "../../../hooks/useProfileSetting";
import {
  getProfileSetting,
  setProfileSetting,
} from "../../../lib/tauri/profile";
import {
  getAutoStart,
  getMinimizeToTray,
  getSortIgnoreArticles,
  getUiZoom,
  getWindowChrome,
  setAutoStart,
  setMinimizeToTray,
  UI_ZOOM_CHANGED_EVENT,
} from "../../../lib/tauri/preferences";
import {
  getBioLanguage,
  getBioSource,
  getDiscordRpcEnabled,
  getNotificationsTrackChange,
  setBioLanguage,
  setBioSource,
  setDiscordRpcEnabled,
  setNotificationsTrackChange,
} from "../../../lib/tauri/integration";
import {
  playerGetAudioSettings,
  playerGetExclusiveOutputState,
  playerSetCrossfade,
  playerSetDsdDop,
  playerSetDsdPrecision,
  playerSetExclusiveOutput,
  playerSetGapless,
  playerSetKeepAwake,
  playerSetMatchSourceRate,
  playerSetMono,
  playerSetNormalize,
  playerSetPauseOnDeviceLoss,
  playerSetReplayGain,
  playerSetReplayGainMode,
  playerSetReplayGainOptions,
  type AudioSettingsSnapshot,
} from "../../../lib/tauri/player";
import {
  playerGetEq,
  playerSetEqEnabled,
  playerSetEqPreset,
  type EqSnapshot,
} from "../../../lib/tauri/eq";
import {
  getDynamicCrossfade,
  getSmartCrossfade,
  setDynamicCrossfade,
  setSmartCrossfade,
} from "../../../lib/tauri/smartCrossfade";
import {
  getVisualizerEnabled,
  setVisualizerEnabled,
} from "../../../lib/tauri/visualizer";
import { getAutoAnalyze, setAutoAnalyze } from "../../../lib/tauri/analysis";
import {
  radioCatalogueStatus,
  setRadioCatalogueLocalFirst,
  type RadioCatalogueStatus,
} from "../../../lib/tauri/webRadioCatalogue";
import {
  getLyricsDefaultDestination,
  getLyricsTranslationLang,
  getPreferLrclib,
  setLyricsDefaultDestination,
  setLyricsTranslationLang,
  setPreferLrclib,
} from "../../../lib/tauri/lyrics";
import { getOfflineMode, setOfflineMode } from "../../../lib/tauri/offline";
import {
  dlnaGetConfig,
  dlnaSetConfig,
  type DlnaConfig,
} from "../../../lib/tauri/dlna";
import {
  mpdGetConfig,
  mpdSetConfig,
  type MpdConfig,
} from "../../../lib/tauri/mpd";
import { remoteGetStatus } from "../../../lib/tauri/remoteServer";
import {
  getClipsInLibrary,
  setClipsInLibrary,
} from "../../../lib/tauri/libraryMedia";
import {
  getUpdateChannel,
  setUpdateChannel,
  UPDATER_RECHECK_EVENT,
} from "../../../lib/tauri/updater";
import {
  displayBackendSetXwayland,
  displayBackendStatus,
  type DisplayBackendStatus,
} from "../../../lib/tauri/displayBackend";
import {
  DEFAULT_BINDINGS,
  loadBindings,
  saveBindings,
  SHORTCUT_ACTIONS,
} from "../../../lib/shortcuts";
import { applyUiZoom } from "../../../hooks/useUiZoom";
import { setSortIgnoreArticles } from "../../../hooks/useSortIgnoreArticles";
import { applyWindowChrome } from "../../../hooks/useWindowChrome";
import { SINGLE_CLICK_PLAY_EVENT } from "../../../hooks/useSingleClickPlay";
import { GENERATOR_ALBUM_MODE_EVENT } from "../../../hooks/useGeneratorAlbumMode";
import { CONTRAST_EVENT } from "../../../hooks/useContrastMode";
import { ACCENT_EVENT } from "../../../hooks/useAccentChoice";
import { parseContrastMode } from "../../../lib/contrast";
import { findSkin } from "../../../lib/skins";
import { findTheme } from "../../../lib/themes";
import { parseAccentChoice, serializeAccentChoice } from "../../../lib/accent";
import {
  COVER_ACTION_KEY,
  PLAYER_BAR_LAYOUT_EVENT,
  PLAYER_BAR_LAYOUT_KEYS,
  type CoverAction,
} from "../../../hooks/usePlayerBarLayout";
import { HI_RES_BADGE_EVENT } from "../../../hooks/useHiResBadgeVisibility";
import { SCROLL_LONG_TITLES_EVENT } from "../../../hooks/useScrollLongTitles";
import {
  IMMERSIVE_PREF_KEYS,
  IMMERSIVE_PREFS_EVENT,
} from "../../../hooks/useImmersivePrefs";
import { COVER_SLIDESHOW_EVENT } from "../../../hooks/useCoverSlideshow";
import { MOTION_COVERS_EVENT } from "../../../hooks/useMotionCovers";
import {
  DESKTOP_LYRICS_STYLE_EVENT,
  parseStyle,
  type DesktopLyricsStyle,
} from "../../../hooks/useDesktopLyricsStyle";
import { ARTIST_HERO_EVENT } from "../../../hooks/useArtistHero";
import { HIDDEN_KPIS_EVENT, parseHidden } from "../../../hooks/useHiddenKpis";
import {
  parseMode as parseWrappedBannerMode,
  WRAPPED_BANNER_EVENT,
} from "../../../hooks/useWrappedBannerVisibility";
import {
  LYRICS_DISABLED_PROVIDERS_EVENT,
  LYRICS_EXCLUDED_GENRES_EVENT,
  parseDisabled,
  parseGenres,
} from "../../../hooks/useLyricsLookupSettings";
import { FULLSCREEN_LYRICS_CENTERING_EVENT } from "../../../hooks/useFullscreenLyricsCentering";
import { ESTIMATED_KARAOKE_EVENT } from "../../../hooks/useEstimatedKaraoke";
import { HELD_NOTE_GLOW_EVENT } from "../../../hooks/useHeldNoteGlow";
import {
  LYRICS_HIGHLIGHT_COLOR_EVENT,
  parseId as parseLyricsHighlightId,
} from "../../../hooks/useLyricsHighlightColor";
import {
  DEFAULT_BITRATE,
  REMOTE_TRANSCODE_EVENT,
  type RemoteTranscodeFormat,
} from "../../../hooks/useRemoteTranscode";

/**
 * The live state a reset has to go through rather than around: the theme
 * and the skin are React state in their providers, which write their own
 * rows and run their own side effects (the view transition, the cached
 * id the next launch reads).
 */
export interface SettingsResetContext {
  profileId: number | null;
  themeId: string;
  /** Resolve once the row is written, reject when it is not. */
  setThemeId: (id: string) => Promise<void>;
  skinId: string;
  setSkinId: (id: string) => Promise<void>;
}

/**
 * Reads that several settings answer from, made once per inspection: the
 * audio engine reports fifteen of them in one snapshot, DLNA and MPD one
 * config each.
 */
interface SharedReads {
  audio(): Promise<AudioSettingsSnapshot>;
  eq(): Promise<EqSnapshot>;
  dlna(): Promise<DlnaConfig>;
  mpd(): Promise<MpdConfig>;
  radio(): Promise<RadioCatalogueStatus>;
  displayBackend(): Promise<DisplayBackendStatus>;
  remoteSignedIn(): Promise<boolean>;
}

function sharedReads(): SharedReads {
  const memo = new Map<string, Promise<unknown>>();
  const once =
    <T>(name: string, read: () => Promise<T>) =>
    (): Promise<T> => {
      let pending = memo.get(name) as Promise<T> | undefined;
      if (!pending) {
        pending = read();
        memo.set(name, pending);
      }
      return pending;
    };
  return {
    audio: once("audio", playerGetAudioSettings),
    eq: once("eq", playerGetEq),
    dlna: once("dlna", dlnaGetConfig),
    mpd: once("mpd", mpdGetConfig),
    radio: once("radio", radioCatalogueStatus),
    displayBackend: once("displayBackend", displayBackendStatus),
    remoteSignedIn: once("remote", async () => {
      try {
        return (await remoteGetStatus()).signed_in;
      } catch {
        // No remote backend in this build, or none bound: the card that
        // shows the setting hides itself the same way.
        return false;
      }
    }),
  };
}

interface SettingAccessor {
  /**
   * The label keys of what differs from the default: one per control
   * that does, so a setting shown as several controls counts each. Empty
   * when everything is at its default — or when this machine never shows
   * the setting, since a count must point at something on the page.
   */
  inspect(ctx: SettingsResetContext, reads: SharedReads): Promise<string[]>;
  /**
   * Put the setting back through the path its control takes, so the
   * side effects follow: the engine is told, the server stops, the
   * window event the mounted hooks re-read on fires.
   */
  reset(ctx: SettingsResetContext): Promise<void>;
}

function sameValue(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/** Lists the backend treats as sets — order is not a change. */
function sameMembers(a: readonly string[], b: readonly string[]): boolean {
  return sameValue([...a].sort(), [...b].sort());
}

function near(a: number, b: number, tolerance = 1e-6): boolean {
  return Math.abs(a - b) <= tolerance;
}

function labelIf(differs: boolean, key: SettingKey): string[] {
  return differs ? [SETTINGS[key].labelKey] : [];
}

/**
 * A setting backed by a backend command of its own — read through its
 * getter, put back through its setter.
 */
function command<K extends SettingKey>(
  key: K,
  read: (reads: SharedReads) => Promise<(typeof SETTINGS)[K]["defaultValue"]>,
  write: (
    value: (typeof SETTINGS)[K]["defaultValue"],
    ctx: SettingsResetContext,
  ) => Promise<unknown>,
  same: (
    a: (typeof SETTINGS)[K]["defaultValue"],
    b: (typeof SETTINGS)[K]["defaultValue"],
  ) => boolean = sameValue,
): SettingAccessor {
  const fallback = SETTINGS[key].defaultValue;
  return {
    inspect: async (_ctx, reads) =>
      labelIf(!same(await read(reads), fallback), key),
    reset: async (ctx) => {
      await write(fallback, ctx);
    },
  };
}

/**
 * A `profile_setting` row the frontend writes itself, with the window
 * event its hook re-reads on. The reset writes the default the way the
 * hook's own setter does and fires the same event, so every mounted
 * consumer re-reads; the backend's `profile-setting:changed` carries it
 * to the other windows.
 */
function profileRow<K extends SettingKey>(
  key: K,
  options: {
    parse: (raw: string | null) => (typeof SETTINGS)[K]["defaultValue"];
    serialize: (value: (typeof SETTINGS)[K]["defaultValue"]) => string;
    valueType: "bool" | "int" | "string" | "json";
    event?: string;
    same?: (
      a: (typeof SETTINGS)[K]["defaultValue"],
      b: (typeof SETTINGS)[K]["defaultValue"],
    ) => boolean;
  },
): SettingAccessor {
  const fallback = SETTINGS[key].defaultValue;
  const same = options.same ?? sameValue;
  return {
    inspect: async (ctx) =>
      labelIf(
        !same(
          options.parse(await getProfileSetting(key, ctx.profileId)),
          fallback,
        ),
        key,
      ),
    reset: async (ctx) => {
      await setProfileSetting(
        key,
        options.serialize(fallback),
        options.valueType,
        ctx.profileId,
      );
      if (options.event) window.dispatchEvent(new CustomEvent(options.event));
    },
  };
}

/** The registry keys whose value is an on/off switch. */
type BooleanSettingKey = {
  [K in SettingKey]: (typeof SETTINGS)[K]["defaultValue"] extends boolean
    ? K
    : never;
}[SettingKey];

/** A switch stored as `"true"` / `"false"`, read as its hook reads it
 *  (`"1"` counts as on too). */
function profileBool(key: BooleanSettingKey, event?: string): SettingAccessor {
  const fallback: boolean = SETTINGS[key].defaultValue;
  return profileRow<BooleanSettingKey>(key, {
    parse: (raw) => parseBoolean(raw, fallback),
    serialize: (value) => (value ? "true" : "false"),
    valueType: "bool",
    event,
  });
}

/**
 * An engine switch, read from its row rather than from the engine's flag.
 * The row is what `restore_profile_audio_settings` reads back at the next
 * launch and on a profile switch, so it is what the setting will be; the
 * flag is only what this session holds. Booleans are read the way that
 * restore reads them — `"1"` / `"0"` are the older spelling, anything else
 * is no answer.
 */
function engineSwitch(
  key: "audio.normalize" | "audio.mono" | "audio.gapless" | "audio.replaygain",
  write: (enabled: boolean) => Promise<unknown>,
): SettingAccessor {
  const fallback: boolean = SETTINGS[key].defaultValue;
  return {
    inspect: async (ctx) => {
      const raw = await getProfileSetting(key, ctx.profileId);
      const value =
        raw === "true" || raw === "1"
          ? true
          : raw === "false" || raw === "0"
            ? false
            : fallback;
      return labelIf(value !== fallback, key);
    },
    reset: async () => {
      await write(fallback);
    },
  };
}

/** Only where the UI offers DoP: every desktop OS, never on mobile. */
export function dopPlatformSupported(): boolean {
  if (typeof navigator === "undefined") return false;
  const ua = navigator.userAgent.toLowerCase();
  return (
    ua.includes("windows") ||
    ua.includes("macintosh") ||
    // Exclude Android, whose UA also contains "linux".
    (ua.includes("linux") && !ua.includes("android"))
  );
}

/** One of the three ReplayGain values the engine takes in a single call:
 *  the other two are sent back as they are. */
function replayGainOption(
  key:
    | "audio.replaygain_preamp"
    | "audio.replaygain_fallback"
    | "audio.replaygain_prevent_clipping",
): SettingAccessor {
  const pick = (s: AudioSettingsSnapshot) =>
    key === "audio.replaygain_preamp"
      ? s.replaygain_preamp_db
      : key === "audio.replaygain_fallback"
        ? s.replaygain_fallback_db
        : s.replaygain_prevent_clipping;
  return {
    inspect: async (_ctx, reads) => {
      const value = pick(await reads.audio());
      const fallback = SETTINGS[key].defaultValue;
      return labelIf(
        typeof value === "number"
          ? !near(value, fallback as number)
          : value !== fallback,
        key,
      );
    },
    reset: async () => {
      // Fresh, not from the inspection: an earlier reset in the same pass
      // may have put one of the other two back already.
      const s = await playerGetAudioSettings();
      await playerSetReplayGainOptions({
        preampDb:
          key === "audio.replaygain_preamp"
            ? SETTINGS[key].defaultValue
            : s.replaygain_preamp_db,
        fallbackDb:
          key === "audio.replaygain_fallback"
            ? SETTINGS[key].defaultValue
            : s.replaygain_fallback_db,
        preventClipping:
          key === "audio.replaygain_prevent_clipping"
            ? SETTINGS[key].defaultValue
            : s.replaygain_prevent_clipping,
      });
    },
  };
}

/** One field of the DLNA config, which is written whole. Enabled comes
 *  first in the registry, so a page reset stops the server before it
 *  touches the name or the port and nothing restarts on the way. */
function dlnaField<F extends keyof DlnaConfig>(
  key: "dlna.enabled" | "dlna.server_name" | "dlna.port",
  field: F,
): SettingAccessor {
  const fallback = SETTINGS[key].defaultValue as DlnaConfig[F];
  return {
    inspect: async (_ctx, reads) =>
      labelIf((await reads.dlna())[field] !== fallback, key),
    reset: async () => {
      const current = await dlnaGetConfig();
      await dlnaSetConfig({ ...current, [field]: fallback });
    },
  };
}

/** Same for MPD. The password is a credential and keeps its value. */
function mpdField<F extends "enabled" | "port">(
  key: "mpd.enabled" | "mpd.port",
  field: F,
): SettingAccessor {
  const fallback = SETTINGS[key].defaultValue as MpdConfig[F];
  return {
    inspect: async (_ctx, reads) =>
      labelIf((await reads.mpd())[field] !== fallback, key),
    reset: async () => {
      const current = await mpdGetConfig();
      await mpdSetConfig({ ...current, [field]: fallback });
    },
  };
}

const DESKTOP_LYRICS_FIELD_LABELS: Record<keyof DesktopLyricsStyle, string> = {
  fontSize: "settings.desktopLyrics.fontSize",
  backgroundOpacity: "settings.desktopLyrics.background",
  textColor: "settings.desktopLyrics.textColor",
  highlightColor: "settings.desktopLyrics.highlightColor",
  outlineWidth: "settings.desktopLyrics.outline",
  showTranslation: "settings.desktopLyrics.showTranslation",
  showNextLine: "settings.desktopLyrics.showNextLine",
  showProgress: "settings.desktopLyrics.showProgress",
  hideOnPause: "settings.desktopLyrics.hideOnPause",
};

function playerBarButton(
  key: (typeof PLAYER_BAR_LAYOUT_KEYS)[keyof typeof PLAYER_BAR_LAYOUT_KEYS],
): SettingAccessor {
  return profileBool(key, PLAYER_BAR_LAYOUT_EVENT);
}

function immersivePref(
  key: (typeof IMMERSIVE_PREF_KEYS)[keyof typeof IMMERSIVE_PREF_KEYS],
): SettingAccessor {
  return profileBool(key, IMMERSIVE_PREFS_EVENT);
}

/**
 * How each registry entry is read and put back. Typed over every key, so
 * an entry added to the registry without one here does not compile.
 */
const ACCESSORS: Record<SettingKey, SettingAccessor> = {
  // ---- General ---------------------------------------------------------
  "app.auto_start": command("app.auto_start", getAutoStart, setAutoStart),
  "app.minimize_to_tray": command(
    "app.minimize_to_tray",
    getMinimizeToTray,
    setMinimizeToTray,
  ),
  "notifications.track_change": command(
    "notifications.track_change",
    getNotificationsTrackChange,
    setNotificationsTrackChange,
  ),

  // ---- Library ---------------------------------------------------------
  // Read at boot only, and written by the Settings switch without an
  // event: the remount after a reset is what refreshes the switch.
  "library.scan_on_start": profileBool("library.scan_on_start"),
  "library.sort_ignore_articles": command(
    "library.sort_ignore_articles",
    getSortIgnoreArticles,
    setSortIgnoreArticles,
  ),
  "audio.auto_analyze": command(
    "audio.auto_analyze",
    getAutoAnalyze,
    setAutoAnalyze,
  ),
  "radio.catalogue.local_first": command(
    "radio.catalogue.local_first",
    async (reads) => (await reads.radio()).localFirst,
    setRadioCatalogueLocalFirst,
  ),

  // ---- Playback and audio ----------------------------------------------
  "audio.gapless": engineSwitch("audio.gapless", playerSetGapless),
  "audio.crossfade_ms": command(
    "audio.crossfade_ms",
    async (reads) => (await reads.audio()).crossfade_ms,
    // The command takes seconds; the slider shows whole ones.
    (ms) => playerSetCrossfade(Math.round(ms / 1000)),
  ),
  "audio.smart_crossfade": command(
    "audio.smart_crossfade",
    getSmartCrossfade,
    setSmartCrossfade,
  ),
  "audio.dynamic_crossfade": command(
    "audio.dynamic_crossfade",
    getDynamicCrossfade,
    setDynamicCrossfade,
  ),
  "ui.single_click_play": profileBool(
    "ui.single_click_play",
    SINGLE_CLICK_PLAY_EVENT,
  ),
  "playback.generator_album_mode": profileBool(
    "playback.generator_album_mode",
    GENERATOR_ALBUM_MODE_EVENT,
  ),
  "audio.pause_on_device_loss": command(
    "audio.pause_on_device_loss",
    async (reads) => (await reads.audio()).pause_on_device_loss,
    playerSetPauseOnDeviceLoss,
  ),
  "playback.keep_awake": command(
    "playback.keep_awake",
    async (reads) => (await reads.audio()).keep_awake,
    (value, ctx) => playerSetKeepAwake(value, ctx.profileId),
  ),
  "audio.normalize": engineSwitch("audio.normalize", playerSetNormalize),
  "audio.replaygain": engineSwitch("audio.replaygain", playerSetReplayGain),
  "audio.replaygain_preamp": replayGainOption("audio.replaygain_preamp"),
  "audio.replaygain_fallback": replayGainOption("audio.replaygain_fallback"),
  "audio.replaygain_mode": command(
    "audio.replaygain_mode",
    async (reads) => (await reads.audio()).replaygain_mode,
    playerSetReplayGainMode,
  ),
  "audio.replaygain_prevent_clipping": replayGainOption(
    "audio.replaygain_prevent_clipping",
  ),
  "audio.eq_enabled": command(
    "audio.eq_enabled",
    async (reads) => (await reads.eq()).enabled,
    playerSetEqEnabled,
  ),
  "audio.eq_bands": {
    inspect: async (_ctx, reads) => {
      const { bands_db } = await reads.eq();
      const flat = SETTINGS["audio.eq_bands"].defaultValue;
      // The card's own tolerance for "this is that preset".
      return labelIf(
        bands_db.some((gain, i) => !near(gain, flat[i] ?? 0, 0.01)),
        "audio.eq_bands",
      );
    },
    // The card's Reset button: the backend's flat preset.
    reset: () => playerSetEqPreset("flat"),
  },
  "audio.exclusive_output": command(
    "audio.exclusive_output",
    async () => (await playerGetExclusiveOutputState()).requested,
    playerSetExclusiveOutput,
  ),
  "audio.match_source_rate": command(
    "audio.match_source_rate",
    async (reads) => (await reads.audio()).match_source_rate,
    playerSetMatchSourceRate,
  ),
  "audio.mono": engineSwitch("audio.mono", playerSetMono),
  "audio.dsd_precision": command(
    "audio.dsd_precision",
    async (reads) =>
      (await reads.audio())
        .dsd_taps as (typeof SETTINGS)["audio.dsd_precision"]["defaultValue"],
    playerSetDsdPrecision,
  ),
  "audio.dsd_dop": {
    inspect: async (_ctx, reads) =>
      labelIf(
        dopPlatformSupported() &&
          (await reads.audio()).dsd_dop !==
            SETTINGS["audio.dsd_dop"].defaultValue,
        "audio.dsd_dop",
      ),
    reset: () => playerSetDsdDop(SETTINGS["audio.dsd_dop"].defaultValue),
  },

  // ---- Appearance ------------------------------------------------------
  // Both read from their row, like the engine switches: the theme swaps
  // on screen before its write, so after a refused write the screen shows
  // a theme the next launch will not. The row is what it will load; the
  // one on screen stands in only while no row exists yet.
  "appearance.theme.id": {
    inspect: async (ctx) => {
      const raw = await getProfileSetting("appearance.theme.id", ctx.profileId);
      return labelIf(
        (raw ? findTheme(raw).id : ctx.themeId) !==
          SETTINGS["appearance.theme.id"].defaultValue,
        "appearance.theme.id",
      );
    },
    reset: async (ctx) =>
      ctx.setThemeId(SETTINGS["appearance.theme.id"].defaultValue),
  },
  "ui.zoom_level": command(
    "ui.zoom_level",
    getUiZoom,
    applyUiZoom,
    // Stored rounded to a tenth; the slider steps in tenths.
    (a, b) => near(a, b, 0.001),
  ),
  "appearance.skin.id": {
    inspect: async (ctx) => {
      const raw = await getProfileSetting("appearance.skin.id", ctx.profileId);
      return labelIf(
        (raw ? findSkin(raw).id : ctx.skinId) !==
          SETTINGS["appearance.skin.id"].defaultValue,
        "appearance.skin.id",
      );
    },
    reset: async (ctx) =>
      ctx.setSkinId(SETTINGS["appearance.skin.id"].defaultValue),
  },
  "appearance.contrast": profileRow("appearance.contrast", {
    parse: parseContrastMode,
    serialize: (value) => value,
    valueType: "string",
    event: CONTRAST_EVENT,
  }),
  "appearance.accent": profileRow("appearance.accent", {
    parse: parseAccentChoice,
    serialize: serializeAccentChoice,
    valueType: "json",
    event: ACCENT_EVENT,
  }),
  "ui.show_lyrics": playerBarButton("ui.show_lyrics"),
  "ui.show_queue": playerBarButton("ui.show_queue"),
  "ui.show_device": playerBarButton("ui.show_device"),
  "ui.show_mini_player": playerBarButton("ui.show_mini_player"),
  "ui.show_immersive": playerBarButton("ui.show_immersive"),
  "ui.show_eq_preset": playerBarButton("ui.show_eq_preset"),
  "ui.show_sleep_timer": playerBarButton("ui.show_sleep_timer"),
  "ui.show_ab_loop": playerBarButton("ui.show_ab_loop"),
  "ui.show_audio_quality_footer": playerBarButton(
    "ui.show_audio_quality_footer",
  ),
  [COVER_ACTION_KEY]: profileRow(COVER_ACTION_KEY, {
    parse: (raw) =>
      raw === "none" || raw === "now_playing" || raw === "immersive"
        ? (raw as CoverAction)
        : SETTINGS[COVER_ACTION_KEY].defaultValue,
    serialize: (value) => value,
    valueType: "string",
    event: PLAYER_BAR_LAYOUT_EVENT,
  }),
  // Its store reads "anything but false / 0" as shown.
  "ui.show_hi_res_badge": profileRow("ui.show_hi_res_badge", {
    parse: (raw) =>
      raw == null
        ? SETTINGS["ui.show_hi_res_badge"].defaultValue
        : raw !== "false" && raw !== "0",
    serialize: (value) => (value ? "true" : "false"),
    valueType: "bool",
    event: HI_RES_BADGE_EVENT,
  }),
  "ui.scroll_long_titles": profileBool(
    "ui.scroll_long_titles",
    SCROLL_LONG_TITLES_EVENT,
  ),
  "ui.window_chrome": {
    inspect: async () => {
      const state = await getWindowChrome();
      return labelIf(
        state.supported &&
          state.chrome !== SETTINGS["ui.window_chrome"].defaultValue,
        "ui.window_chrome",
      );
    },
    reset: async () => {
      await applyWindowChrome(SETTINGS["ui.window_chrome"].defaultValue);
    },
  },
  "display.xwayland": {
    // Not when `GDK_BACKEND` decides: the card's switch is locked then.
    inspect: async (_ctx, reads) => {
      const status = await reads.displayBackend();
      return labelIf(
        status.available &&
          !status.set_by_user &&
          status.xwayland !== SETTINGS["display.xwayland"].defaultValue,
        "display.xwayland",
      );
    },
    reset: async () => {
      await displayBackendSetXwayland(
        SETTINGS["display.xwayland"].defaultValue,
      );
    },
  },
  "ui.visualizer": command(
    "ui.visualizer",
    getVisualizerEnabled,
    setVisualizerEnabled,
  ),
  "immersive.merged_lyrics": immersivePref("immersive.merged_lyrics"),
  "immersive.use_native_fullscreen": immersivePref(
    "immersive.use_native_fullscreen",
  ),
  "immersive.hide_empty_lyrics": immersivePref("immersive.hide_empty_lyrics"),
  "ui.cover_slideshow": profileBool(
    "ui.cover_slideshow",
    COVER_SLIDESHOW_EVENT,
  ),
  "ui.motion_covers": profileBool("ui.motion_covers", MOTION_COVERS_EVENT),
  "ui.desktop_lyrics_style": {
    inspect: async (ctx) => {
      const style = parseStyle(
        await getProfileSetting("ui.desktop_lyrics_style", ctx.profileId),
      );
      const fallback = SETTINGS["ui.desktop_lyrics_style"].defaultValue;
      return (
        Object.keys(DESKTOP_LYRICS_FIELD_LABELS) as Array<
          keyof DesktopLyricsStyle
        >
      )
        .filter((field) => style[field] !== fallback[field])
        .map((field) => DESKTOP_LYRICS_FIELD_LABELS[field]);
    },
    reset: async (ctx) => {
      await setProfileSetting(
        "ui.desktop_lyrics_style",
        JSON.stringify(SETTINGS["ui.desktop_lyrics_style"].defaultValue),
        "json",
        ctx.profileId,
      );
      window.dispatchEvent(new CustomEvent(DESKTOP_LYRICS_STYLE_EVENT));
    },
  },
  "ui.artist_hero": profileBool("ui.artist_hero", ARTIST_HERO_EVENT),
  "stats.hidden_kpis": profileRow("stats.hidden_kpis", {
    parse: parseHidden,
    serialize: (ids) => JSON.stringify(ids),
    valueType: "json",
    event: HIDDEN_KPIS_EVENT,
    same: sameMembers,
  }),
  "wrapped.banner_visibility": profileRow("wrapped.banner_visibility", {
    parse: parseWrappedBannerMode,
    serialize: (value) => value,
    valueType: "string",
    event: WRAPPED_BANNER_EVENT,
  }),

  // ---- Images and lyrics -----------------------------------------------
  "metadata.bio_source": command(
    "metadata.bio_source",
    getBioSource,
    setBioSource,
  ),
  "metadata.bio_language": command(
    "metadata.bio_language",
    getBioLanguage,
    setBioLanguage,
  ),
  "lyrics.translation_lang": command(
    "lyrics.translation_lang",
    getLyricsTranslationLang,
    setLyricsTranslationLang,
  ),
  "lyrics.prefer_lrclib": command(
    "lyrics.prefer_lrclib",
    getPreferLrclib,
    setPreferLrclib,
  ),
  "lyrics.disabled_providers": profileRow("lyrics.disabled_providers", {
    parse: parseDisabled,
    serialize: (ids) => JSON.stringify(ids),
    valueType: "json",
    event: LYRICS_DISABLED_PROVIDERS_EVENT,
    same: sameMembers,
  }),
  "lyrics.excluded_genres": profileRow("lyrics.excluded_genres", {
    parse: parseGenres,
    serialize: (genres) => JSON.stringify(genres),
    valueType: "json",
    event: LYRICS_EXCLUDED_GENRES_EVENT,
    same: sameMembers,
  }),
  "lyrics.default_destination": command(
    "lyrics.default_destination",
    getLyricsDefaultDestination,
    setLyricsDefaultDestination,
  ),
  "lyrics.fullscreen_sync_centered": profileBool(
    "lyrics.fullscreen_sync_centered",
    FULLSCREEN_LYRICS_CENTERING_EVENT,
  ),
  "lyrics.estimate_words": profileBool(
    "lyrics.estimate_words",
    ESTIMATED_KARAOKE_EVENT,
  ),
  "lyrics.held_notes": profileBool("lyrics.held_notes", HELD_NOTE_GLOW_EVENT),
  "ui.lyrics_highlight_color": profileRow("ui.lyrics_highlight_color", {
    parse: parseLyricsHighlightId,
    serialize: (value) => value ?? "",
    valueType: "string",
    event: LYRICS_HIGHLIGHT_COLOR_EVENT,
  }),

  // ---- Connections -----------------------------------------------------
  "network.offline_mode": command(
    "network.offline_mode",
    getOfflineMode,
    setOfflineMode,
  ),
  "integrations.discord_rpc": command(
    "integrations.discord_rpc",
    getDiscordRpcEnabled,
    setDiscordRpcEnabled,
  ),
  // Ahead of the format: its default is the one of the format in force,
  // which the format's own reset is about to turn off.
  "remote.transcode.bitrate": {
    inspect: async (ctx, reads) => {
      if (!(await reads.remoteSignedIn())) return [];
      const format = await readTranscodeFormat(ctx);
      if (format === "off") return [];
      const raw = await getProfileSetting(
        "remote.transcode.bitrate",
        ctx.profileId,
      );
      const stored = Number.parseInt(raw ?? "", 10);
      return labelIf(
        Number.isFinite(stored) && stored !== DEFAULT_BITRATE[format],
        "remote.transcode.bitrate",
      );
    },
    reset: async (ctx) => {
      const format = await readTranscodeFormat(ctx);
      if (format === "off") return;
      await setProfileSetting(
        "remote.transcode.bitrate",
        String(DEFAULT_BITRATE[format]),
        "int",
        ctx.profileId,
      );
      window.dispatchEvent(new CustomEvent(REMOTE_TRANSCODE_EVENT));
    },
  },
  "remote.transcode.format": {
    inspect: async (ctx, reads) =>
      labelIf(
        (await reads.remoteSignedIn()) &&
          (await readTranscodeFormat(ctx)) !==
            SETTINGS["remote.transcode.format"].defaultValue,
        "remote.transcode.format",
      ),
    reset: async (ctx) => {
      await setProfileSetting(
        "remote.transcode.format",
        SETTINGS["remote.transcode.format"].defaultValue,
        "string",
        ctx.profileId,
      );
      window.dispatchEvent(new CustomEvent(REMOTE_TRANSCODE_EVENT));
    },
  },
  "dlna.enabled": dlnaField("dlna.enabled", "enabled"),
  "dlna.server_name": dlnaField("dlna.server_name", "server_name"),
  "dlna.port": dlnaField("dlna.port", "port"),
  "mpd.enabled": mpdField("mpd.enabled", "enabled"),
  "mpd.port": mpdField("mpd.port", "port"),

  // ---- Keyboard shortcuts ----------------------------------------------
  "ui.shortcuts": {
    inspect: async () => {
      const bindings = await loadBindings();
      return SHORTCUT_ACTIONS.filter(
        (action) => bindings[action] !== DEFAULT_BINDINGS[action],
      ).map((action) => `settings.shortcuts.actions.${action}`);
    },
    // The card's "Reset all": it stores the overrides, none here.
    reset: () => saveBindings({ ...DEFAULT_BINDINGS }),
  },

  // ---- Storage and backups ---------------------------------------------
  "media.clips_in_library": command(
    "media.clips_in_library",
    getClipsInLibrary,
    setClipsInLibrary,
  ),

  // ---- Maintenance -----------------------------------------------------
  "updater.channel": command(
    "updater.channel",
    getUpdateChannel,
    async (channel) => {
      await setUpdateChannel(channel);
      // What the card does after a switch: look again on the channel now
      // in force.
      window.dispatchEvent(new CustomEvent(UPDATER_RECHECK_EVENT));
    },
  ),
};

async function readTranscodeFormat(
  ctx: SettingsResetContext,
): Promise<RemoteTranscodeFormat> {
  const raw = await getProfileSetting("remote.transcode.format", ctx.profileId);
  return raw === "mp3" || raw === "opus"
    ? raw
    : SETTINGS["remote.transcode.format"].defaultValue;
}

function keysOf(category: SettingsCategory): SettingKey[] {
  return SETTING_KEYS.filter((key) => SETTINGS[key].category === category);
}

/** One line of the reset dialog: a label, and how many of its controls
 *  differ when it stands for several. */
export interface ModifiedSetting {
  labelKey: string;
  count: number;
}

export interface CategoryModifications {
  /** Total number of differing controls. */
  total: number;
  items: ModifiedSetting[];
  /** The registry entries that differ — what a reset will touch. */
  keys: SettingKey[];
}

async function inspectCategory(
  category: SettingsCategory,
  ctx: SettingsResetContext,
): Promise<CategoryModifications> {
  const reads = sharedReads();
  const keys = keysOf(category);
  const results = await Promise.all(
    keys.map(async (key) => {
      try {
        return await ACCESSORS[key].inspect(ctx, reads);
      } catch (err) {
        // One unreadable setting must not hide the others; it simply is
        // not counted, and so not reset either.
        console.error(`[settingsReset] read ${key} failed`, err);
        return [];
      }
    }),
  );
  const items: ModifiedSetting[] = [];
  const modifiedKeys: SettingKey[] = [];
  let total = 0;
  results.forEach((labels, i) => {
    if (labels.length === 0) return;
    modifiedKeys.push(keys[i]);
    for (const labelKey of labels) {
      total += 1;
      const existing = items.find((item) => item.labelKey === labelKey);
      if (existing) existing.count += 1;
      else items.push({ labelKey, count: 1 });
    }
  });
  return { total, items, keys: modifiedKeys };
}

/**
 * Put every setting of `category` that differs back to its default, one
 * after the other, in registry order. Each goes through its own control's
 * path; a failure is collected rather than stopping the rest.
 *
 * Reads the category afresh rather than trusting the count on screen: the
 * dialog may have been open while something changed.
 */
export async function resetCategory(
  category: SettingsCategory,
  ctx: SettingsResetContext,
): Promise<{ attempted: number; failed: SettingKey[] }> {
  const { keys } = await inspectCategory(category, ctx);
  const failed: SettingKey[] = [];
  for (const key of keys) {
    try {
      await ACCESSORS[key].reset(ctx);
    } catch (err) {
      console.error(`[settingsReset] reset ${key} failed`, err);
      failed.push(key);
    }
  }
  return { attempted: keys.length, failed };
}

/** The context the accessors need, from the providers that own it. */
export function useSettingsResetContext(): SettingsResetContext {
  const { activeProfile } = useProfile();
  const { theme, setThemeId } = useTheme();
  const { skin, setSkinId } = useSkin();
  const profileId = activeProfile?.id ?? null;
  return useMemo(
    () => ({
      profileId,
      themeId: theme.id,
      setThemeId: (id: string) => setThemeId(id),
      skinId: skin.id,
      setSkinId,
    }),
    [profileId, theme.id, setThemeId, skin.id, setSkinId],
  );
}

/** Window events that may mean a value of the page moved. */
const WINDOW_EVENTS = [SETTING_WRITTEN_EVENT, UI_ZOOM_CHANGED_EVENT];
/** Backend events that may: every `set_profile_setting` (from any
 *  window), and the equalizer, which the player bar's preset menu can
 *  change while Settings is open. */
const BACKEND_EVENTS = ["profile-setting:changed", "player:eq"];

/**
 * Which settings of `category` differ from their default, kept current
 * while the page is open. `null` until the first inspection lands.
 *
 * Re-inspects on every announced write, debounced: one toggle can fire two
 * signals (its window event and the backend's), and a slider fires one per
 * step.
 */
export function useCategoryModifications(
  category: SettingsCategory,
  ctx: SettingsResetContext,
): { modifications: CategoryModifications | null; refresh: () => void } {
  const [state, setState] = useState<{
    category: SettingsCategory;
    value: CategoryModifications;
  } | null>(null);
  const sequence = useRef(0);
  const timer = useRef<number | null>(null);
  const ctxRef = useRef(ctx);
  useEffect(() => {
    ctxRef.current = ctx;
  });

  const inspect = useCallback(() => {
    const mine = ++sequence.current;
    void inspectCategory(category, ctxRef.current).then((value) => {
      // Only the newest inspection lands: an older one may have read
      // before a write the newer one saw.
      if (mine === sequence.current) setState({ category, value });
    });
  }, [category]);

  const refresh = useCallback(() => {
    if (timer.current != null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => {
      timer.current = null;
      inspect();
    }, 150);
  }, [inspect]);

  // Inspect on mount, on a category change, and when the profile, the
  // theme or the skin — which the accessors read from context — move.
  useEffect(() => {
    inspect();
  }, [inspect, ctx.profileId, ctx.themeId, ctx.skinId]);

  useEffect(() => {
    for (const name of WINDOW_EVENTS) window.addEventListener(name, refresh);
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    for (const name of BACKEND_EVENTS) {
      listen(name, refresh)
        .then((unlisten) => {
          if (disposed) unlisten();
          else unlisteners.push(unlisten);
        })
        .catch(() => {
          // Not in a Tauri webview: nothing would emit it either.
        });
    }
    return () => {
      disposed = true;
      for (const name of WINDOW_EVENTS) {
        window.removeEventListener(name, refresh);
      }
      for (const unlisten of unlisteners) unlisten();
      if (timer.current != null) {
        window.clearTimeout(timer.current);
        timer.current = null;
      }
    };
  }, [refresh]);

  return {
    modifications: state?.category === category ? state.value : null,
    refresh,
  };
}

/** A reset the Settings view has just remounted for. */
export interface CompletedReset {
  category: SettingsCategory;
  /** How many of its settings could not be put back. */
  failed: number;
}

export interface SettingsResetScopeValue {
  /**
   * Called once a reset has put back at least one setting of
   * `category`, whether or not others failed. The Settings view answers
   * by mounting its content afresh: much of what a page shows is state
   * the view and its cards read once from their commands (the audio
   * engine's switches, the DLNA and MPD configs, the cards that own a
   * value), and a fresh mount is the one refresh that reaches every one
   * of them. Skipping it after a partial failure left those controls on
   * values the database no longer held.
   */
  onCategoryReset: (category: SettingsCategory, failed: number) => void;
  /**
   * The reset the current mount follows, kept above the remount so the
   * fresh header can put the focus back and, after a partial failure,
   * reopen its dialog on what is left. `null` once taken.
   */
  completed: CompletedReset | null;
  /** Mark `completed` as taken. */
  consume: () => void;
}

export const SettingsResetScope = createContext<SettingsResetScopeValue>({
  onCategoryReset: () => {},
  completed: null,
  consume: () => {},
});

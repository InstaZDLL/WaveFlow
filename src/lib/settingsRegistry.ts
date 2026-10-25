import type { SettingsCategory } from "../components/views/settings/settingsCatalog";
import type { CoverAction } from "../hooks/usePlayerBarLayout";
import type { DesktopLyricsStyle } from "../hooks/useDesktopLyricsStyle";
import type { LyricsHighlightId } from "../hooks/useLyricsHighlightColor";
import type { RemoteTranscodeFormat } from "../hooks/useRemoteTranscode";
import type { StatsKpiId } from "../hooks/useHiddenKpis";
import type { WrappedBannerMode } from "../hooks/useWrappedBannerVisibility";
import { DEFAULT_ACCENT_CHOICE, type AccentChoice } from "./accent";
import { DEFAULT_CONTRAST_MODE, type ContrastMode } from "./contrast";
import { DEFAULT_EFFECTS_LEVEL, type EffectsLevel } from "./effects";
import { DEFAULT_SKIN_ID, type SkinId } from "./skins";
import { DEFAULT_THEME_ID } from "./themes";
import type { BioLanguage, BioSource } from "./tauri/integration";
import type { LyricsDestination, LyricsProvider } from "./tauri/lyrics";
import type { DsdPrecisionTaps, ReplayGainMode } from "./tauri/player";
import type { ShortcutAction } from "./shortcuts";
import type { UpdateChannel } from "./tauri/updater";

/**
 * Where a preference is kept.
 *
 * - `profile` — the active profile's `profile_setting` table.
 * - `app` — the app-wide `app_setting` table, shared by every profile.
 * - `device` — outside both databases: the OS autostart entry, the
 *   display-backend file read before a window exists.
 */
export type SettingScope = "profile" | "app" | "device";

/**
 * The kind of value. For a `profile_setting` written from the frontend it
 * is also the marker stored beside it, which the preference hook passes
 * through; for one written by a backend command of its own it only
 * describes the value.
 */
export type SettingValueType = "bool" | "int" | "float" | "string" | "json";

export interface SettingDefinition<T> {
  /** The Settings category whose page shows it, and whose header counts
   *  it among the settings that differ from their default. */
  category: SettingsCategory;
  scope: SettingScope;
  valueType: SettingValueType;
  defaultValue: T;
  /** Names the setting in the reset dialog: the title its row already
   *  shows. Several entries of one card share their card's title. */
  labelKey: string;
}

function setting<T>(
  category: SettingsCategory,
  scope: SettingScope,
  valueType: SettingValueType,
  defaultValue: T,
  labelKey: string,
): SettingDefinition<T> {
  return { category, scope, valueType, defaultValue, labelKey };
}

/** The player-bar buttons, in the order their card lists them. */
const PLAYER_BAR_BUTTON_LABEL = "settings.playerBarLayout.title";

/**
 * Every user preference the Settings view shows, with its default.
 *
 * This is the one place a default is written down. The preference hooks
 * and the Settings view read theirs from here, and so does the header of
 * each Settings category, which counts the entries of its page that differ
 * from this value and offers to put them back. A default changed here
 * changes all three; a second copy of it somewhere else is what would let
 * the count and the screen disagree.
 *
 * Keyed by storage key (the `profile_setting` / `app_setting` key), so a
 * hook reads `SETTINGS["ui.artist_hero"].defaultValue` next to the key it
 * already names. The few entries that are not one row say so.
 *
 * What is NOT here, on purpose: anything that is not a preference. Account
 * credentials and sign-ins (Last.fm, ListenBrainz, the remote server, the
 * MPD password), library folders, installed plugins, caches and where they
 * live, backups and their configuration, the profile's name and avatar,
 * window positions, and the interface language, which onboarding asks for
 * and whose only "default" is whatever the system language happens to be.
 *
 * The backend keeps its own fallbacks for the values it reads before the
 * frontend exists (the audio engine, Discord, DLNA…). They are in Rust and
 * cannot import this file; each entry below agrees with its Rust
 * counterpart, which is what "the effective default" means for the count.
 */
export const SETTINGS = {
  // ---- General ---------------------------------------------------------
  /** The OS login item, through the autostart plugin — no database row. */
  "app.auto_start": setting(
    "general",
    "device",
    "bool",
    false,
    "settings.autoStart.title",
  ),
  "app.minimize_to_tray": setting(
    "general",
    "app",
    "bool",
    true,
    "settings.minimizeToTray.title",
  ),
  "notifications.track_change": setting(
    "general",
    "app",
    "bool",
    false,
    "settings.integrations.notifications.title",
  ),

  // ---- Library ---------------------------------------------------------
  "library.scan_on_start": setting(
    "library",
    "profile",
    "bool",
    false,
    "settings.scanOnStart.title",
  ),
  "library.sort_ignore_articles": setting(
    "library",
    "app",
    "bool",
    true,
    "settings.sortIgnoreArticles.title",
  ),
  "audio.auto_analyze": setting(
    "library",
    "profile",
    "bool",
    false,
    "settings.analyze.autoTitle",
  ),
  "radio.catalogue.local_first": setting(
    "library",
    "app",
    "bool",
    false,
    "settings.radioCatalogue.localFirst",
  ),

  // ---- Playback and audio ----------------------------------------------
  "audio.gapless": setting(
    "playback",
    "profile",
    "bool",
    true,
    "settings.gapless.title",
  ),
  "audio.crossfade_ms": setting(
    "playback",
    "profile",
    "int",
    0,
    "settings.crossfade.title",
  ),
  "audio.smart_crossfade": setting(
    "playback",
    "profile",
    "bool",
    false,
    "settings.smartCrossfade.title",
  ),
  "audio.dynamic_crossfade": setting(
    "playback",
    "profile",
    "bool",
    false,
    "settings.dynamicCrossfade.title",
  ),
  "ui.single_click_play": setting(
    "playback",
    "profile",
    "bool",
    false,
    "settings.singleClickPlay.title",
  ),
  "playback.autoplay": setting(
    "playback",
    "profile",
    "bool",
    false,
    "settings.autoplay.title",
  ),
  "playback.generator_album_mode": setting(
    "playback",
    "profile",
    "bool",
    false,
    "settings.generatorAlbumMode.title",
  ),
  "audio.pause_on_device_loss": setting(
    "playback",
    "profile",
    "bool",
    true,
    "settings.pauseOnDeviceLoss.title",
  ),
  "playback.keep_awake": setting(
    "playback",
    "profile",
    "bool",
    true,
    "settings.keepAwake.title",
  ),
  "audio.normalize": setting(
    "playback",
    "profile",
    "bool",
    false,
    "settings.normalize.title",
  ),
  "audio.replaygain": setting(
    "playback",
    "profile",
    "bool",
    false,
    "settings.replayGain.title",
  ),
  "audio.replaygain_preamp": setting(
    "playback",
    "profile",
    "float",
    0,
    "settings.replayGain.preamp.title",
  ),
  "audio.replaygain_fallback": setting(
    "playback",
    "profile",
    "float",
    0,
    "settings.replayGain.fallback.title",
  ),
  "audio.replaygain_mode": setting<ReplayGainMode>(
    "playback",
    "profile",
    "string",
    "auto",
    "settings.replayGain.mode.title",
  ),
  "audio.replaygain_prevent_clipping": setting(
    "playback",
    "profile",
    "bool",
    true,
    "settings.replayGain.preventClipping.title",
  ),
  "audio.eq_enabled": setting(
    "playback",
    "profile",
    "bool",
    false,
    "settings.equalizer.title",
  ),
  /** The six band gains, in dB. Flat is the backend's `flat` preset. */
  "audio.eq_bands": setting<readonly number[]>(
    "playback",
    "profile",
    "json",
    [0, 0, 0, 0, 0, 0],
    "settings.equalizer.title",
  ),
  "audio.exclusive_output": setting(
    "playback",
    "profile",
    "bool",
    false,
    "settings.exclusive.title",
  ),
  "audio.match_source_rate": setting(
    "playback",
    "profile",
    "bool",
    false,
    "settings.matchSourceRate.title",
  ),
  "audio.mono": setting(
    "playback",
    "profile",
    "bool",
    false,
    "settings.mono.title",
  ),
  "audio.balance": setting(
    "playback",
    "profile",
    "int",
    0,
    "settings.stereoImage.balance.title",
  ),
  "audio.stereo_width": setting(
    "playback",
    "profile",
    "int",
    100,
    "settings.stereoImage.width.title",
  ),
  "audio.dsd_precision": setting<DsdPrecisionTaps>(
    "playback",
    "profile",
    "int",
    256,
    "settings.dsdPrecision.title",
  ),
  "audio.dsd_dop": setting(
    "playback",
    "profile",
    "bool",
    false,
    "settings.dsdDop.title",
  ),

  // ---- Appearance ------------------------------------------------------
  "appearance.theme.id": setting<string>(
    "appearance",
    "profile",
    "string",
    DEFAULT_THEME_ID,
    "settings.appearance.theme.title",
  ),
  "ui.zoom_level": setting(
    "appearance",
    "app",
    "float",
    1,
    "settings.uiZoom.title",
  ),
  "appearance.skin.id": setting<SkinId>(
    "appearance",
    "profile",
    "string",
    DEFAULT_SKIN_ID,
    "settings.appearance.skin.title",
  ),
  "appearance.contrast": setting<ContrastMode>(
    "appearance",
    "profile",
    "string",
    DEFAULT_CONTRAST_MODE,
    "settings.contrast.title",
  ),
  "appearance.effects": setting<EffectsLevel>(
    "appearance",
    "profile",
    "string",
    DEFAULT_EFFECTS_LEVEL,
    "settings.effects.title",
  ),
  "appearance.accent": setting<AccentChoice>(
    "appearance",
    "profile",
    "json",
    DEFAULT_ACCENT_CHOICE,
    "settings.appearance.accent.title",
  ),
  // The defaults match the bar as it was before it could be customised,
  // so nobody's bar changed on the upgrade that introduced the card.
  "ui.show_lyrics": setting(
    "appearance",
    "profile",
    "bool",
    true,
    PLAYER_BAR_BUTTON_LABEL,
  ),
  "ui.show_queue": setting(
    "appearance",
    "profile",
    "bool",
    true,
    PLAYER_BAR_BUTTON_LABEL,
  ),
  "ui.show_device": setting(
    "appearance",
    "profile",
    "bool",
    true,
    PLAYER_BAR_BUTTON_LABEL,
  ),
  "ui.show_mini_player": setting(
    "appearance",
    "profile",
    "bool",
    true,
    PLAYER_BAR_BUTTON_LABEL,
  ),
  "ui.show_immersive": setting(
    "appearance",
    "profile",
    "bool",
    true,
    PLAYER_BAR_BUTTON_LABEL,
  ),
  "ui.show_eq_preset": setting(
    "appearance",
    "profile",
    "bool",
    false,
    PLAYER_BAR_BUTTON_LABEL,
  ),
  "ui.show_sleep_timer": setting(
    "appearance",
    "profile",
    "bool",
    false,
    PLAYER_BAR_BUTTON_LABEL,
  ),
  "ui.show_ab_loop": setting(
    "appearance",
    "profile",
    "bool",
    false,
    PLAYER_BAR_BUTTON_LABEL,
  ),
  "ui.show_audio_quality_footer": setting(
    "appearance",
    "profile",
    "bool",
    false,
    PLAYER_BAR_BUTTON_LABEL,
  ),
  "ui.cover_action": setting<CoverAction>(
    "appearance",
    "profile",
    "string",
    "immersive",
    "settings.playerBarLayout.coverAction.title",
  ),
  "ui.show_hi_res_badge": setting(
    "appearance",
    "profile",
    "bool",
    true,
    "settings.hiResBadge.title",
  ),
  "ui.scroll_long_titles": setting(
    "appearance",
    "profile",
    "bool",
    true,
    "settings.scrollTitles.title",
  ),
  /** Linux and macOS only; elsewhere the card, and the count, skip it.
   *  `adwaita` is Linux on GTK's Wayland backend only. */
  "ui.window_chrome": setting<"system" | "adwaita" | "app">(
    "appearance",
    "app",
    "string",
    "system",
    "settings.windowChrome.title",
  ),
  /** `display-backend.json`, read before a window exists. Linux on a
   *  Wayland session only. */
  "display.xwayland": setting(
    "appearance",
    "device",
    "bool",
    false,
    "settings.xwayland.title",
  ),
  "ui.visualizer": setting(
    "appearance",
    "profile",
    "bool",
    true,
    "settings.visualizer.title",
  ),
  "immersive.merged_lyrics": setting(
    "appearance",
    "profile",
    "bool",
    true,
    "settings.immersiveView.mergedLyrics.title",
  ),
  "immersive.use_native_fullscreen": setting(
    "appearance",
    "profile",
    "bool",
    false,
    "settings.immersiveView.nativeFullscreen.title",
  ),
  "immersive.hide_empty_lyrics": setting(
    "appearance",
    "profile",
    "bool",
    false,
    "settings.immersiveView.hideEmptyLyrics.title",
  ),
  "ui.cover_slideshow": setting(
    "appearance",
    "profile",
    "bool",
    false,
    "settings.coverSlideshow.title",
  ),
  "ui.motion_covers": setting(
    "appearance",
    "profile",
    "bool",
    true,
    "settings.motionCovers.title",
  ),
  /** One JSON row, nine controls; each control that differs counts. */
  "ui.desktop_lyrics_style": setting<DesktopLyricsStyle>(
    "appearance",
    "profile",
    "json",
    {
      fontSize: 36,
      textColor: "#ffffff",
      highlightColor: "#34d399",
      outlineWidth: 2,
      backgroundOpacity: 0,
      showTranslation: true,
      showNextLine: true,
      hideOnPause: false,
      showProgress: false,
    },
    "desktopLyrics.title",
  ),
  "ui.artist_hero": setting(
    "appearance",
    "profile",
    "bool",
    true,
    "settings.artistHero.title",
  ),
  "stats.hidden_kpis": setting<readonly StatsKpiId[]>(
    "appearance",
    "profile",
    "json",
    [],
    "settings.statsKpis.title",
  ),
  "wrapped.banner_visibility": setting<WrappedBannerMode>(
    "appearance",
    "profile",
    "string",
    "auto",
    "settings.wrappedBanner.title",
  ),

  // ---- Images and lyrics -----------------------------------------------
  "metadata.bio_source": setting<BioSource>(
    "media",
    "app",
    "string",
    "lastfm",
    "settings.artistBio.title",
  ),
  "metadata.bio_language": setting<BioLanguage>(
    "media",
    "app",
    "string",
    "en",
    "settings.artistBio.languageLabel",
  ),
  /** `null` is "no translation"; the backend deletes the row for it. */
  "lyrics.translation_lang": setting<string | null>(
    "media",
    "profile",
    "string",
    null,
    "settings.lyricsTranslation.title",
  ),
  "lyrics.prefer_lrclib": setting(
    "media",
    "profile",
    "bool",
    false,
    "settings.lyricsPreferLrclib.title",
  ),
  "lyrics.disabled_providers": setting<readonly LyricsProvider[]>(
    "media",
    "profile",
    "json",
    ["genius"],
    "settings.lyricsProviders.title",
  ),
  "lyrics.excluded_genres": setting<readonly string[]>(
    "media",
    "profile",
    "json",
    ["Instrumental", "Lo-fi"],
    "settings.lyricsExcludedGenres.title",
  ),
  "lyrics.default_destination": setting<LyricsDestination>(
    "media",
    "app",
    "string",
    "tag",
    "settings.lyricsDestination.title",
  ),
  "lyrics.fullscreen_sync_centered": setting(
    "media",
    "profile",
    "bool",
    false,
    "settings.fullscreenLyricsCentering.title",
  ),
  "lyrics.estimate_words": setting(
    "media",
    "profile",
    "bool",
    false,
    "settings.lyricsEstimateWords.title",
  ),
  "lyrics.held_notes": setting(
    "media",
    "profile",
    "bool",
    true,
    "settings.lyricsHeldNotes.title",
  ),
  /** `null` follows the accent; stored as an empty string. */
  "ui.lyrics_highlight_color": setting<LyricsHighlightId | null>(
    "media",
    "profile",
    "string",
    null,
    "settings.lyricsHighlightColor.title",
  ),

  // ---- Connections -----------------------------------------------------
  "network.offline_mode": setting(
    "integrations",
    "app",
    "bool",
    false,
    "settings.offlineMode.title",
  ),
  /** On: the backend treats a missing row as on, which is the choice the
   *  presence worker makes at boot. */
  "integrations.discord_rpc": setting(
    "integrations",
    "app",
    "bool",
    true,
    "settings.integrations.discord.title",
  ),
  /** `0` is "never chosen": the rate then follows the format's own
   *  default, which is why the count compares against that instead.
   *  Listed ahead of the format, so a page reset puts the rate back to
   *  the default of the format in force before turning the format off. */
  "remote.transcode.bitrate": setting(
    "integrations",
    "profile",
    "int",
    0,
    "settings.remoteTranscode.bitrateLabel",
  ),
  "remote.transcode.format": setting<RemoteTranscodeFormat>(
    "integrations",
    "profile",
    "string",
    "off",
    "settings.remoteTranscode.title",
  ),
  "dlna.enabled": setting(
    "integrations",
    "app",
    "bool",
    false,
    "settings.integrations.dlna.title",
  ),
  "dlna.server_name": setting(
    "integrations",
    "app",
    "string",
    "WaveFlow",
    "settings.integrations.dlna.serverName",
  ),
  /** `0` lets the OS pick a free port. */
  "dlna.port": setting(
    "integrations",
    "app",
    "int",
    0,
    "settings.integrations.dlna.port",
  ),
  "mpd.enabled": setting(
    "integrations",
    "app",
    "bool",
    false,
    "settings.integrations.mpd.title",
  ),
  "mpd.port": setting(
    "integrations",
    "app",
    "int",
    6600,
    "settings.integrations.mpd.port",
  ),

  // ---- Keyboard shortcuts ----------------------------------------------
  /** The user's overrides only; a binding left out is its default from
   *  `DEFAULT_BINDINGS`. Each overridden action counts, under its own
   *  name in the reset dialog. */
  "ui.shortcuts": setting<Partial<Record<ShortcutAction, string>>>(
    "shortcuts",
    "profile",
    "string",
    {},
    "settings.organization.groups.keyboard",
  ),

  // ---- Storage and backups ---------------------------------------------
  "media.clips_in_library": setting(
    "data",
    "app",
    "bool",
    false,
    "settings.clipsInLibrary.title",
  ),

  // ---- Maintenance -----------------------------------------------------
  "updater.channel": setting<UpdateChannel>(
    "diagnostics",
    "app",
    "string",
    "stable",
    "settings.updateChannel.title",
  ),
} as const;

export type SettingKey = keyof typeof SETTINGS;

/** The registry's keys in page order, the order the reset dialog lists
 *  them in. */
export const SETTING_KEYS = Object.keys(SETTINGS) as SettingKey[];

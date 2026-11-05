import {
  getActiveProfile,
  getProfileSetting,
  setProfileSetting,
} from "./tauri/profile";
import type { ViewId } from "../types";
import type {
  SettingsCategory,
  SettingsGroupId,
} from "../components/views/settings/settingsCatalog";

/**
 * What the Home's "First steps" card lists: features a new user would
 * not find by browsing, each ticked by the first use of it.
 *
 * The flags are per profile (`profile_setting`), one row per feature
 * (`discovery.done.<id>`), so two features used at the same moment never
 * race over a shared document. They are state the app keeps, not
 * preferences: like `release_radar.plugin_notice_seen` they stay out of
 * the settings registry, or Settings would count them as "modified" and
 * offer to reset them.
 */
export const DISCOVERIES = [
  "immersive",
  "miniPlayer",
  "desktopLyrics",
  "smartPlaylist",
  "enrichment",
  "tagWorkshop",
  "wrapped",
  "plugin",
] as const;

export type DiscoveryId = (typeof DISCOVERIES)[number];

const DONE_PREFIX = "discovery.done.";
/** The card was closed: it never comes back for this profile. */
export const DISCOVERY_HIDDEN_KEY = "discovery.hidden";
/** What the profile's own data already shows was ticked once, on its
 *  first visit after the update. A profile with no library yet (a new
 *  one) is marked seeded with nothing ticked. */
export const DISCOVERY_SEEDED_KEY = "discovery.seeded";

/** Broadcast after a flag is written, so the card re-reads. */
export const DISCOVERY_CHANGED_EVENT = "waveflow:discovery-changed";

export function discoveryKey(id: DiscoveryId): string {
  return `${DONE_PREFIX}${id}`;
}

/**
 * Tick a feature as used, from the place where it is used, in the profile
 * active at that moment: its id is pinned on the read and the write, so a
 * switch in between makes the write refused rather than land in the next
 * profile. An item already ticked is left alone — a use after the first
 * costs a read, not a write and a re-read of the card. A failure only
 * leaves the item unticked, so it is logged and dropped.
 */
export function noteDiscovery(id: DiscoveryId): void {
  discoveryOnSuccess(id)();
}

/**
 * For a use that takes a while (an install, a save): capture the profile
 * now, when the user acts, and tick it once the use succeeded — in that
 * profile, even if another became active meanwhile (the write is then
 * refused, which is right: the use belonged to the profile left).
 */
export function discoveryOnSuccess(id: DiscoveryId): () => void {
  const profile = getActiveProfile();
  // Settled here so a rejection is never unhandled when no tick follows.
  profile.catch(() => undefined);
  return () => {
    const key = discoveryKey(id);
    profile
      .then(async (active) => {
        if (!active) return;
        if ((await getProfileSetting(key, active.id)) === "true") return;
        await setProfileSetting(key, "true", "bool", active.id);
        window.dispatchEvent(new CustomEvent(DISCOVERY_CHANGED_EVENT));
      })
      .catch((err) => console.error(`[discovery] note ${id} failed`, err));
  };
}

/**
 * The contextual tips: each shown once per profile, the flag written as
 * soon as it is drawn (as the Release Radar notice does), so a tip that is
 * never closed does not come back every visit.
 */
export const TIPS = ["lyricsSources", "tagWorkshop"] as const;
export type TipId = (typeof TIPS)[number];

export function tipKey(id: TipId): string {
  return `tips.${id}.seen`;
}

/** Read a tip's flag for one profile; `true` when it was already shown. */
export async function tipSeen(id: TipId, profileId: number): Promise<boolean> {
  return (await getProfileSetting(tipKey(id), profileId)) === "true";
}

export function markTipSeen(id: TipId, profileId: number): Promise<void> {
  return setProfileSetting(tipKey(id), "true", "bool", profileId);
}

/**
 * Open a view from a place that has no navigation callback — a tip in a
 * panel, a first-steps item, the onboarding's link to a setting. The
 * layout listens and pushes the entry, as it does for the tag workshop.
 */
export const OPEN_VIEW_EVENT = "waveflow:open-view";

export interface OpenViewRequest {
  view: ViewId;
  /** With `view: "settings"`: the category to open on. */
  settingsCategory?: SettingsCategory;
  /** With a category: the group of it to scroll to. */
  settingsGroup?: SettingsGroupId;
  /** With `view: "about"`: scroll to the changelog. */
  aboutChangelog?: boolean;
}

export function openView(request: OpenViewRequest): void {
  window.dispatchEvent(
    new CustomEvent<OpenViewRequest>(OPEN_VIEW_EVENT, { detail: request }),
  );
}

/** Ask the layout to run the onboarding again (Settings → General). */
export const REPLAY_ONBOARDING_EVENT = "waveflow:replay-onboarding";

export function replayOnboarding(): void {
  window.dispatchEvent(new CustomEvent(REPLAY_ONBOARDING_EVENT));
}

/** Ask the sidebar to open the smart playlist editor, which it owns. */
export const OPEN_SMART_PLAYLIST_EDITOR_EVENT =
  "waveflow:open-smart-playlist-editor";

export function openSmartPlaylistEditor(): void {
  window.dispatchEvent(new CustomEvent(OPEN_SMART_PLAYLIST_EDITOR_EVENT));
}

/**
 * The player bar tour: shown once per profile, offered at the end of the
 * journey and started again from Settings → General. The flag is state the
 * app keeps, not a preference, so it stays out of the settings registry.
 */
export const PLAYER_TOUR_SEEN_KEY = "tour.player_bar.seen";

export function playerTourSeen(profileId: number): Promise<boolean> {
  return getProfileSetting(PLAYER_TOUR_SEEN_KEY, profileId).then(
    (raw) => raw === "true",
  );
}

export function markPlayerTourSeen(profileId: number): Promise<void> {
  return setProfileSetting(PLAYER_TOUR_SEEN_KEY, "true", "bool", profileId);
}

/** Ask the layout to start the player bar tour. */
export const START_PLAYER_TOUR_EVENT = "waveflow:start-player-tour";

export function startPlayerTour(): void {
  window.dispatchEvent(new CustomEvent(START_PLAYER_TOUR_EVENT));
}

/** Ask the layout to open the "What's new" panel. */
export const OPEN_WHATS_NEW_EVENT = "waveflow:open-whats-new";

export function openWhatsNew(): void {
  window.dispatchEvent(new CustomEvent(OPEN_WHATS_NEW_EVENT));
}

/** Ask the sidebar to open the playlist import dialog, which it owns. */
export const OPEN_PLAYLIST_IMPORT_EVENT = "waveflow:open-playlist-import";

export function openPlaylistImport(): void {
  window.dispatchEvent(new CustomEvent(OPEN_PLAYLIST_IMPORT_EVENT));
}

/**
 * Ask for the warning that comes before podcasts turn on. It only asks: the
 * dialog's own confirmation is the one thing that enables them (RFC-009), so a
 * row in Settings, a page of What's new or a button on the podcasts page can
 * lead here and none of them can do more.
 */
export const OPEN_PODCAST_WARNING_EVENT = "waveflow:open-podcast-warning";

export interface PodcastWarningRequest {
  /** Go to the podcasts page once accepted, for a request made elsewhere. */
  openPage?: boolean;
}

export function openPodcastWarning(request: PodcastWarningRequest = {}): void {
  window.dispatchEvent(
    new CustomEvent<PodcastWarningRequest>(OPEN_PODCAST_WARNING_EVENT, {
      detail: request,
    }),
  );
}

/** Ask for an OPML list to be read: the layout runs it, so the report still
 *  reaches someone who left the page during a long list. */
export const IMPORT_PODCAST_OPML_EVENT = "waveflow:import-podcast-opml";

export function importPodcastOpml(path: string): void {
  window.dispatchEvent(
    new CustomEvent<string>(IMPORT_PODCAST_OPML_EVENT, { detail: path }),
  );
}

/**
 * Ask for the lyrics editor. The panel that owns it may not be mounted yet
 * (it is being opened by the same gesture), so the request waits here for
 * it to take: the event reaches a panel already there, its mount finds the
 * flag otherwise.
 */
export const OPEN_LYRICS_EDITOR_EVENT = "waveflow:open-lyrics-editor";

let lyricsEditorRequested = false;

export function requestLyricsEditor(): void {
  lyricsEditorRequested = true;
  window.dispatchEvent(new CustomEvent(OPEN_LYRICS_EDITOR_EVENT));
}

/** Whether the editor was asked for. Answers once. */
export function takeLyricsEditorRequest(): boolean {
  const requested = lyricsEditorRequested;
  lyricsEditorRequested = false;
  return requested;
}

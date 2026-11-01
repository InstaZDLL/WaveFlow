import {
  useCallback,
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { getVersion } from "@tauri-apps/api/app";

import { useAnyModalOpen } from "./useModalA11y";
import { useLibrary } from "./useLibrary";
import { useProfile } from "./useProfile";
import { useProfileBooleanSetting } from "./useProfileSetting";
import { OPEN_WHATS_NEW_EVENT } from "../lib/discovery";
import { SETTINGS } from "../lib/settingsRegistry";
import { getProfileSetting } from "../lib/tauri/profile";
import { getWhatsNewMarker, setWhatsNewMarker } from "../lib/tauri/preferences";
import {
  latestRelease,
  markerShouldAdvance,
  pendingRelease,
  type WhatsNewRelease,
} from "../lib/whatsNew";

export const WHATS_NEW_AUTO_OPEN_EVENT = "waveflow:whats-new-auto-open";
export const WHATS_NEW_BUTTON_EVENT = "waveflow:whats-new-button";

/** How long the way has to stay clear before the panel opens by itself: a
 *  dialog closing is often the first of several, and something opened next
 *  should not find the panel in its place. */
const CLEAR_FOR_MS = 800;

/** Per profile: whether the panel opens by itself after an update. */
export function useWhatsNewAutoOpen() {
  return useProfileBooleanSetting({
    key: "ui.whats_new_auto_open",
    defaultValue: SETTINGS["ui.whats_new_auto_open"].defaultValue,
    event: WHATS_NEW_AUTO_OPEN_EVENT,
    label: "useWhatsNewAutoOpen",
  });
}

/** Per profile: whether the top bar shows the button that reopens it. */
export function useWhatsNewButton() {
  return useProfileBooleanSetting({
    key: "ui.whats_new_button",
    defaultValue: SETTINGS["ui.whats_new_button"].defaultValue,
    event: WHATS_NEW_BUTTON_EVENT,
    label: "useWhatsNewButton",
  });
}

/** The release this version can show on demand: `null` hides what would
 *  open an empty panel (Settings' row, About's link). */
export function useWhatsNewRelease(): WhatsNewRelease | null {
  const [release, setRelease] = useState<WhatsNewRelease | null>(null);
  useEffect(() => {
    let cancelled = false;
    getVersion()
      .then((version) => {
        if (!cancelled) setRelease(latestRelease(version));
      })
      .catch((err) => console.error("[useWhatsNewRelease] version", err));
    return () => {
      cancelled = true;
    };
  }, []);
  return release;
}

function subscribeWindowFocus(listener: () => void) {
  window.addEventListener("focus", listener);
  window.addEventListener("blur", listener);
  document.addEventListener("visibilitychange", listener);
  return () => {
    window.removeEventListener("focus", listener);
    window.removeEventListener("blur", listener);
    document.removeEventListener("visibilitychange", listener);
  };
}

/** Whether the window is in front of someone. A launch `--minimized` by the
 *  OS sits hidden in the tray, and WebView2 reports a hidden page as visible,
 *  so only the focus tells. */
export function useWindowFocused(): boolean {
  return useSyncExternalStore(
    subscribeWindowFocus,
    () => document.visibilityState === "visible" && document.hasFocus(),
    () => true,
  );
}

export interface WhatsNewState {
  /** The release the button can open, `null` when there is none. */
  release: WhatsNewRelease | null;
  /** A release nobody on this installation has been shown yet. */
  unseen: boolean;
  /** The release the panel holds, kept after closing for its exit motion. */
  shown: WhatsNewRelease | null;
  open: boolean;
  openPanel: () => void;
  closePanel: () => void;
}

/**
 * Decides when the "What's new" panel opens.
 *
 * Once per launch, when the active profile and its libraries are known, the
 * version running is compared with the marker `app_setting` holds:
 *
 * - No marker and nothing here but a first run (one profile, no library, the
 *   onboarding never finished): a new installation. No panel, since the
 *   journey has just told all of it, and the marker is set so the next
 *   launch does not take this one for an update.
 * - No marker and a profile that shows a past (a 1.x, which never wrote one):
 *   an update, and everything is new.
 * - A release between the marker and this version: it is pending. It opens by
 *   itself when the way is clear, unless this profile asked not to; the
 *   button then carries a dot until it is opened by hand.
 * - Nothing to tell (a correction release): the marker moves and that is all.
 *
 * The marker moves when a pending release is actually shown, not when it is
 * found: closing the app behind another dialog, or under a profile that does
 * not want the panel, leaves it to be seen later. It is app-wide because an
 * update arrives once per installation, so a second profile does not see the
 * panel again.
 *
 * `blocked` is what the layout knows and a modal count does not: the journey
 * about to open, the tour.
 */
export function useWhatsNew(blocked: boolean): WhatsNewState {
  const {
    libraries,
    isLoading: isLibraryLoading,
    loadedProfileId,
  } = useLibrary();
  const { activeProfile, profiles, isLoading: isProfileLoading } = useProfile();
  const autoOpen = useWhatsNewAutoOpen();
  const anyModalOpen = useAnyModalOpen();
  const focused = useWindowFocused();

  const [release, setRelease] = useState<WhatsNewRelease | null>(null);
  const [pending, setPending] = useState<WhatsNewRelease | null>(null);
  const [shown, setShown] = useState<WhatsNewRelease | null>(null);
  const [open, setOpen] = useState(false);
  const versionRef = useRef<string | null>(null);
  const evaluated = useRef(false);

  useEffect(() => {
    if (evaluated.current || isProfileLoading || isLibraryLoading) return;
    if (!activeProfile || loadedProfileId !== activeProfile.id) return;
    evaluated.current = true;

    const profileId = activeProfile.id;
    const holdsMusic = libraries.some(
      (library) => library.folder_count > 0 || library.track_count > 0,
    );
    const severalProfiles = profiles.length > 1;
    (async () => {
      const version = await getVersion();
      versionRef.current = version;
      setRelease(latestRelease(version));

      const marker = await getWhatsNewMarker();
      if (marker == null) {
        const finishedJourney =
          (await getProfileSetting("onboarding.dismissed", profileId)) ===
          "true";
        if (!holdsMusic && !severalProfiles && !finishedJourney) {
          await setWhatsNewMarker(version);
          return;
        }
      }
      const next = pendingRelease(marker, version);
      if (next) setPending(next);
      else if (markerShouldAdvance(marker, version)) {
        await setWhatsNewMarker(version);
      }
    })().catch((err) => console.error("[useWhatsNew] evaluate failed", err));
  }, [
    activeProfile,
    profiles,
    libraries,
    isProfileLoading,
    isLibraryLoading,
    loadedProfileId,
  ]);

  const showPanel = useCallback((next: WhatsNewRelease, unseen: boolean) => {
    setShown(next);
    setOpen(true);
    if (!unseen) return;
    setPending(null);
    const version = versionRef.current;
    if (version) {
      setWhatsNewMarker(version).catch((err) =>
        console.error("[useWhatsNew] write marker failed", err),
      );
    }
  }, []);

  const wayClear =
    pending != null &&
    autoOpen.ready &&
    autoOpen.value &&
    !blocked &&
    !anyModalOpen &&
    !open &&
    focused;

  useEffect(() => {
    if (!wayClear || !pending) return;
    const timer = window.setTimeout(
      () => showPanel(pending, true),
      CLEAR_FOR_MS,
    );
    return () => window.clearTimeout(timer);
  }, [wayClear, pending, showPanel]);

  const openPanel = useCallback(() => {
    if (pending) showPanel(pending, true);
    else if (release) showPanel(release, false);
  }, [pending, release, showPanel]);

  const closePanel = useCallback(() => setOpen(false), []);

  useEffect(() => {
    window.addEventListener(OPEN_WHATS_NEW_EVENT, openPanel);
    return () => window.removeEventListener(OPEN_WHATS_NEW_EVENT, openPanel);
  }, [openPanel]);

  return {
    release,
    unseen: pending != null,
    shown,
    open,
    openPanel,
    closePanel,
  };
}

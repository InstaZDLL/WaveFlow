import { useCallback } from "react";
import { SETTINGS } from "../lib/settingsRegistry";
import { useProfileBooleanSetting } from "./useProfileSetting";

/** Its own broadcast channel, as every preference has. */
export const PODCASTS_ENABLED_EVENT = "waveflow:podcasts-enabled";

export interface PodcastsSwitch {
  enabled: boolean;
  ready: boolean;
  /**
   * The one way to turn podcasts on. Only the confirmation of the warning dialog
   * calls this: RFC-009 makes accepting it explicit, so neither the Settings row,
   * nor opening an OPML file, nor onboarding or What's new may.
   */
  acceptWarning: () => Promise<void>;
  /** Asks for nothing: subscriptions, positions and downloads stay. */
  turnOff: () => Promise<void>;
}

/**
 * Whether this profile follows podcasts. Off by default, and off again in a
 * profile imported or restored, since the acceptance belongs to the person on
 * this machine. The backend reads the same key before every request and refuses
 * when it is off, and its timer wakes when the key is written.
 */
export function usePodcastsEnabled(): PodcastsSwitch {
  const { value, ready, setValue } = useProfileBooleanSetting({
    key: "podcasts.enabled",
    defaultValue: SETTINGS["podcasts.enabled"].defaultValue,
    event: PODCASTS_ENABLED_EVENT,
    label: "usePodcastsEnabled",
  });
  const acceptWarning = useCallback(() => setValue(true), [setValue]);
  const turnOff = useCallback(() => setValue(false), [setValue]);
  return { enabled: value, ready, acceptWarning, turnOff };
}

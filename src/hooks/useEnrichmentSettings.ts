import { useProfileBooleanSetting } from "./useProfileSetting";
import { SETTINGS } from "../lib/settingsRegistry";

/** The engine's switch and the four fields it may fill. Each key has its
 *  own broadcast channel: sharing one would make a write to either re-read
 *  the other and clobber its in-flight optimistic value. */
export const ENRICHMENT_SETTING_EVENTS = {
  "enrichment.enabled": "waveflow:enrichment-enabled",
  "enrichment.fill_genres": "waveflow:enrichment-fill-genres",
  "enrichment.fill_dates": "waveflow:enrichment-fill-dates",
  "enrichment.fill_covers": "waveflow:enrichment-fill-covers",
  "enrichment.fill_credits": "waveflow:enrichment-fill-credits",
} as const;

export type EnrichmentSettingKey = keyof typeof ENRICHMENT_SETTING_EVENTS;

export interface EnrichmentToggle {
  value: boolean;
  ready: boolean;
  setValue: (next: boolean) => Promise<void>;
}

/**
 * One per-profile switch of the enrichment engine. The backend reads the
 * same key at the start of every pass (`commands::enrichment::read_fields`),
 * so a change applies from the next pass with nothing to push.
 */
export function useEnrichmentToggle(
  key: EnrichmentSettingKey,
): EnrichmentToggle {
  const { value, ready, setValue } = useProfileBooleanSetting({
    key,
    defaultValue: SETTINGS[key].defaultValue,
    event: ENRICHMENT_SETTING_EVENTS[key],
    label: `useEnrichmentToggle(${key})`,
  });
  return { value, ready, setValue };
}

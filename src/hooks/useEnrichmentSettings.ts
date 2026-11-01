import { useCallback, useState } from "react";
import { useProfileBooleanSetting } from "./useProfileSetting";
import { SETTINGS } from "../lib/settingsRegistry";
import { cancelEnrichment, runEnrichment } from "../lib/tauri/enrichment";
import { discoveryOnSuccess } from "../lib/discovery";

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

export interface EnrichmentSwitch {
  enabled: boolean;
  ready: boolean;
  /** The last switch that failed, said as the error read. */
  error: string | null;
  toggle: () => Promise<void>;
}

/**
 * The engine's on/off switch as Settings → Library has it, for every place
 * that shows it (that card, the onboarding): the setting is written first,
 * then turning it on starts a pass straight away rather than waiting for
 * the next scan, and turning it off stops the running one. Turning it on
 * ticks the Home's first steps, in the profile it was clicked in.
 */
export function useEnrichmentSwitch(): EnrichmentSwitch {
  const enabled = useEnrichmentToggle("enrichment.enabled");
  const [error, setError] = useState<string | null>(null);
  const { value, setValue } = enabled;
  const toggle = useCallback(async () => {
    const next = !value;
    const tick = discoveryOnSuccess("enrichment");
    setError(null);
    try {
      await setValue(next);
      if (next) {
        await runEnrichment();
        tick();
      } else await cancelEnrichment();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, [value, setValue]);
  return { enabled: value, ready: enabled.ready, error, toggle };
}

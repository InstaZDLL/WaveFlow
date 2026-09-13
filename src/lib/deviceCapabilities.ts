import type { DeviceCapabilities } from "./tauri/player";

/**
 * The rate tiers a capability sheet groups by (#593).
 *
 * A column of numbers from 44.1 to 384 tells an expert something and a
 * normal user nothing; the tiers tell both. The boundaries are the
 * industry's own families — the CD and DVD rates, then each doubling.
 */
export type RateTierKey = "cd" | "hiRes" | "studio" | "ultra";

export interface RateTier {
  key: RateTierKey;
  /** The rates of this tier the device actually accepts, ascending. */
  rates: number[];
}

const TIER_ORDER: RateTierKey[] = ["cd", "hiRes", "studio", "ultra"];

function tierOf(rate: number): RateTierKey {
  if (rate <= 48_000) return "cd";
  if (rate <= 96_000) return "hiRes";
  if (rate <= 192_000) return "studio";
  return "ultra";
}

/** Group accepted rates into the named tiers, empty tiers omitted. */
export function rateTiers(rates: number[]): RateTier[] {
  const grouped = new Map<RateTierKey, number[]>();
  for (const rate of [...rates].sort((a, b) => a - b)) {
    const key = tierOf(rate);
    const bucket = grouped.get(key);
    if (bucket) bucket.push(rate);
    else grouped.set(key, [rate]);
  }
  return TIER_ORDER.filter((key) => grouped.has(key)).map((key) => ({
    key,
    rates: grouped.get(key) ?? [],
  }));
}

/**
 * The deepest format the device takes, in bits of real audio.
 *
 * `null` when nothing was learned, which is a different statement from
 * "16 bit" and has to stay distinguishable.
 */
export function bestFormatBits(caps: DeviceCapabilities): number | null {
  if (caps.formats.length === 0) return null;
  return caps.formats.reduce((best, format) => Math.max(best, format.bits), 0);
}

/** The top rate the device takes, `null` when nothing was learned. */
export function topSampleRate(caps: DeviceCapabilities): number | null {
  if (caps.sample_rates.length === 0) return null;
  return caps.sample_rates.reduce((best, rate) => Math.max(best, rate), 0);
}

/** "192" / "44.1" — the compact spelling the player already uses. */
export function rateKHz(hz: number): string {
  return (hz / 1000).toFixed(1).replace(/\.0$/, "");
}

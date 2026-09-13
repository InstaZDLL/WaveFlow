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

/** A depth and a rate the device accepts **together**. */
export interface FormatPair {
  bits: number;
  rate: number;
}

/**
 * The best pair the device actually accepts: the deepest format, and the
 * top rate that format runs at.
 *
 * A pair, not two maxima. A DAC that takes 32-bit to 96 kHz and 24-bit
 * to 192 kHz would otherwise be summarised as "32 bit · 192 kHz", which
 * it has never accepted — and the Hi-Res marker would be decided on that
 * same invented combination.
 *
 * `null` when nothing was learned, which is a different statement from
 * "16 bit" and has to stay distinguishable.
 */
export function bestFormatPair(caps: DeviceCapabilities): FormatPair | null {
  let best: FormatPair | null = null;
  for (const format of caps.formats) {
    const rate = format.sample_rates.reduce((top, r) => Math.max(top, r), 0);
    if (rate <= 0) continue;
    if (
      best == null ||
      format.bits > best.bits ||
      (format.bits === best.bits && rate > best.rate)
    ) {
      best = { bits: format.bits, rate };
    }
  }
  return best;
}

/** "192" / "44.1" — the compact spelling the player already uses. */
export function rateKHz(hz: number): string {
  return (hz / 1000).toFixed(1).replace(/\.0$/, "");
}

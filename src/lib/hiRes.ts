import { fileKind } from "./audioQuality";

/**
 * Industry definition of "Hi-Res Audio": ≥ 24-bit source bit depth at
 * ≥ 44.1 kHz sample rate. Lossy formats that don't expose a bit
 * depth automatically fail the check, which is what we want — a
 * 320 kbps MP3 isn't Hi-Res no matter how high its rate is.
 */
export function isHiRes(
  bitDepth: number | null | undefined,
  sampleRate: number | null | undefined,
): boolean {
  if (bitDepth == null || sampleRate == null) return false;
  return bitDepth >= 24 && sampleRate >= 44100;
}

/**
 * DSD-specific badge label, derived from the scanner's `codec`
 * field. The backend stamps `"DSD64"`, `"DSD128"`, … per multiple
 * of 44.1 kHz. Returns `null` for non-DSD tracks so callers can
 * fall back to the standard Hi-Res badge.
 */
export function dsdLabel(codec: string | null | undefined): string | null {
  if (!codec) return null;
  return codec.startsWith("DSD") ? codec : null;
}

/**
 * The quality badge a file earns, best first:
 *
 * - `dsd` — a DSD file, labelled by its rate (`DSD64`, `DSD128`…);
 * - `hiRes` — ≥ 24-bit at ≥ 44.1 kHz ([`isHiRes`]);
 * - `lossless` — a lossless file that is neither, the CD-quality tier
 *   (16-bit FLAC, ALAC, WAV, AIFF, APE, WavPack), by the same rule as the
 *   backend's `lossless_track_sql!` ([`fileKind`]).
 *
 * `probableFakeLossless` withholds the lossless tier from a file the
 * analysis caught stopping at a lossy encoder's wall: the label would
 * vouch for what the file is not. It is only known where the caller has
 * read the track's analysis; elsewhere it is `undefined` and the tier is
 * judged on the codec alone. Hi-Res and DSD are left as they were.
 */
export type QualityBadge =
  { kind: "dsd"; label: string } | { kind: "hiRes" } | { kind: "lossless" };

export function qualityBadge(
  bitDepth: number | null | undefined,
  sampleRate: number | null | undefined,
  codec: string | null | undefined,
  probableFakeLossless?: boolean,
): QualityBadge | null {
  const dsd = dsdLabel(codec);
  if (dsd) return { kind: "dsd", label: dsd };
  if (isHiRes(bitDepth, sampleRate)) return { kind: "hiRes" };
  if (!probableFakeLossless && fileKind(codec, bitDepth) === "lossless") {
    return { kind: "lossless" };
  }
  return null;
}

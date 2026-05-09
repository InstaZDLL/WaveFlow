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

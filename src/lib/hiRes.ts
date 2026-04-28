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

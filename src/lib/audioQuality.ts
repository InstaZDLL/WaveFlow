/**
 * Words and figures for the audio-quality strip and its pipeline cards.
 *
 * Everything here describes values the engine or the scanner published
 * (`QueueTrackPayload`, `PlayerStateSnapshot`); nothing is measured or
 * guessed on this side.
 */

/** How a file keeps the recording, in the terms a listener uses. */
export type FileKind = "lossless" | "lossy" | "dsd";

/**
 * Whether a file is lossless, lossy or DSD, from the codec label the
 * scanner stamps (`scanner::extract::file_type_label`).
 *
 * The lossless half is the backend's own rule, `lossless_track_sql!` in
 * `analysis/bandwidth.rs`: ALAC rides in the same MP4 container as AAC
 * and is labelled `AAC`, and only its bit depth tells it apart, since no
 * lossy codec reports one. A change there belongs here too. A codec
 * neither list knows (a stream with no label, a plugin's custom label)
 * gets no verdict rather than a guess.
 */
export function fileKind(
  codec: string | null | undefined,
  bitDepth: number | null | undefined,
): FileKind | null {
  if (!codec) return null;
  if (codec.startsWith("DSD")) return "dsd";
  if (["FLAC", "WAV", "AIFF", "PCM", "APE", "WavPack"].includes(codec)) {
    return "lossless";
  }
  if (codec === "AAC") return bitDepth != null ? "lossless" : "lossy";
  if (["MP3", "Vorbis", "Opus", "Speex"].includes(codec)) return "lossy";
  return null;
}

function numberFormat(language: string, maxDigits: number): Intl.NumberFormat {
  try {
    return new Intl.NumberFormat(language, {
      maximumFractionDigits: maxDigits,
    });
  } catch {
    // An unknown tag (a legacy alias) must not cost the figure.
    return new Intl.NumberFormat("en", { maximumFractionDigits: maxDigits });
  }
}

/**
 * A rate in Hz as "44.1 kHz" / "44,1 kHz" in the reader's language. Up
 * to three decimals, so 22 050 Hz reads "22.05 kHz" rather than rounding
 * onto a rate it is not; the usual rates print as they always did.
 */
export function formatKHz(
  hz: number | null | undefined,
  language: string,
): string | null {
  if (hz == null || hz <= 0) return null;
  return `${numberFormat(language, 3).format(hz / 1000)} kHz`;
}

/** A bitrate as kb/s under 1000, Mb/s with up to two decimals above. */
export function formatBitrate(
  kbps: number | null | undefined,
  language: string,
): string | null {
  if (kbps == null || kbps <= 0) return null;
  if (kbps >= 1000) {
    const mbps = kbps / 1000;
    return `${numberFormat(language, mbps >= 10 ? 1 : 2).format(mbps)} Mb/s`;
  }
  return `${kbps} kb/s`;
}

/**
 * A channel count as the layout name speakers are sold under (5.1, 7.1).
 * Mono and stereo are words, so they are left to the caller to
 * translate: this returns `null` for them.
 */
export function surroundLayout(channels: number): string | null {
  switch (channels) {
    case 3:
      return "3.0";
    case 4:
      return "4.0";
    case 5:
      return "5.0";
    case 6:
      return "5.1";
    case 7:
      return "6.1";
    case 8:
      return "7.1";
    default:
      return null;
  }
}

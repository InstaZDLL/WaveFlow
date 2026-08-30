import { useCallback } from "react";
import { useProfileSetting } from "./useProfileSetting";

const FORMAT_KEY = "remote.transcode.format";
const BITRATE_KEY = "remote.transcode.bitrate";

/** Broadcast so every mounted consumer re-reads after a write. */
export const REMOTE_TRANSCODE_EVENT = "waveflow:remote-transcode";

/**
 * What the server should encode a remote stream as.
 *
 * `"off"` is the default and means the original bytes. Transcoding trades
 * audio quality for bandwidth, which is a trade nobody should be opted into
 * — and it applies to the **remote source only**: a local file is already on
 * the machine, so re-encoding it would cost quality and buy nothing.
 */
export type RemoteTranscodeFormat = "off" | "mp3" | "opus";

const DEFAULT_FORMAT: RemoteTranscodeFormat = "off";

/** The server's own defaults, mirrored so the picker opens on the value the
 *  server would have chosen rather than on an arbitrary one. */
export const DEFAULT_BITRATE: Record<RemoteTranscodeFormat, number> = {
  off: 0,
  mp3: 192,
  opus: 128,
};

/** The server answers 400 outside this range, and the backend clamps to it. */
export const MIN_BITRATE = 32;
export const MAX_BITRATE = 512;

/** Offered rates. Opus is useful well below where MP3 stops being listenable,
 *  which is the whole reason both are offered rather than one. */
export const BITRATE_CHOICES: Record<
  Exclude<RemoteTranscodeFormat, "off">,
  ReadonlyArray<number>
> = {
  mp3: [128, 192, 256, 320],
  opus: [64, 96, 128, 160, 192],
};

/** Anything unrecognised — including a value written by a newer build —
 *  plays the original rather than guessing at a codec. Same rule as the
 *  backend's parser, deliberately: the two read one row. */
function parseFormat(raw: string | null): RemoteTranscodeFormat {
  return raw === "mp3" || raw === "opus" ? raw : DEFAULT_FORMAT;
}

export interface RemoteTranscode {
  format: RemoteTranscodeFormat;
  bitrate: number;
  /** `false` until the active profile's values have been read. */
  ready: boolean;
  setFormat: (next: RemoteTranscodeFormat) => void;
  setBitrate: (next: number) => void;
}

/**
 * Per-profile preference for transcoding the remote source.
 *
 * Two rows rather than one encoded string: the backend reads them from SQL
 * when it builds the stream URL, and a composite value would make that read
 * parse a format of our invention. Concurrency, profile isolation and
 * rollback all live in [`useProfileSetting`](./useProfileSetting.ts).
 */
export function useRemoteTranscode(): RemoteTranscode {
  const formatSetting = useProfileSetting<RemoteTranscodeFormat>({
    key: FORMAT_KEY,
    defaultValue: DEFAULT_FORMAT,
    parse: parseFormat,
    serialize: (value) => value,
    valueType: "string",
    event: REMOTE_TRANSCODE_EVENT,
    label: "useRemoteTranscode.format",
  });

  const bitrateSetting = useProfileSetting<number>({
    key: BITRATE_KEY,
    defaultValue: UNSET,
    parse: parseBitrate,
    serialize: (value) => String(value),
    valueType: "int",
    event: REMOTE_TRANSCODE_EVENT,
    label: "useRemoteTranscode.bitrate",
  });

  const setFormatValue = formatSetting.setValue;
  const setBitrateValue = bitrateSetting.setValue;
  // With no row stored, the rate is the *current codec's* default — which is
  // what the backend resolves too. A fixed fallback would have the picker
  // showing one number while the URL carried another, and the picker is the
  // only place the user can see what is being asked for.
  const stored = bitrateSetting.value;
  const bitrate =
    stored === UNSET
      ? DEFAULT_BITRATE[formatSetting.value] || DEFAULT_BITRATE.mp3
      : stored;

  const setFormat = useCallback(
    (next: RemoteTranscodeFormat) => {
      void setFormatValue(next);
      // Switching codec carries the bitrate over, and the two scales do not
      // mean the same thing — 320 is transparent-ish for MP3 and wasteful
      // for Opus. Land on the new codec's own default unless the current
      // value is one it actually offers.
      if (next !== "off") {
        const offered = BITRATE_CHOICES[next];
        if (!offered.includes(bitrate)) {
          void setBitrateValue(DEFAULT_BITRATE[next]);
        }
      }
    },
    [setFormatValue, setBitrateValue, bitrate],
  );

  const setBitrate = useCallback(
    (next: number) => {
      void setBitrateValue(parseBitrate(String(next)));
    },
    [setBitrateValue],
  );

  return {
    format: formatSetting.value,
    bitrate,
    ready: formatSetting.ready && bitrateSetting.ready,
    setFormat,
    setBitrate,
  };
}

/**
 * Sentinel for "no row stored yet", distinguishable from every rate the
 * server accepts because it sits below the minimum one. Needed because the
 * default rate depends on the chosen codec, which this parser cannot see.
 */
const UNSET = 0;

/** Clamped rather than rejected: a row outside the server's range would
 *  otherwise turn into a 400 on every remote track. */
function parseBitrate(raw: string | null): number {
  const parsed = Number.parseInt(raw ?? "", 10);
  if (!Number.isFinite(parsed)) return UNSET;
  return Math.min(MAX_BITRATE, Math.max(MIN_BITRATE, parsed));
}

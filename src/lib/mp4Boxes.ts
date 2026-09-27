/**
 * Just enough ISO-BMFF reading to hand a fragmented MP4 to MediaSource:
 * whether a file is fragmented, and the RFC 6381 codec string its video
 * track declares. Everything here reads the start of the file only — the
 * `moov` box, which Apple's motion covers and ordinary clips alike put
 * before their media.
 */

function fourcc(bytes: Uint8Array, at: number): string {
  return String.fromCharCode(
    bytes[at],
    bytes[at + 1],
    bytes[at + 2],
    bytes[at + 3],
  );
}

function u32(bytes: Uint8Array, at: number): number {
  return (
    ((bytes[at] << 24) |
      (bytes[at + 1] << 16) |
      (bytes[at + 2] << 8) |
      bytes[at + 3]) >>>
    0
  );
}

function hex2(n: number): string {
  return n.toString(16).padStart(2, "0");
}

/** Offset of the first occurrence of `tag` in `bytes`, or -1. */
function find(bytes: Uint8Array, tag: string, from = 0): number {
  const [a, b, c, d] = [0, 1, 2, 3].map((i) => tag.charCodeAt(i));
  for (let i = from; i + 3 < bytes.length; i++) {
    if (
      bytes[i] === a &&
      bytes[i + 1] === b &&
      bytes[i + 2] === c &&
      bytes[i + 3] === d
    ) {
      return i;
    }
  }
  return -1;
}

/** The `moov` box's bytes, walking the top-level boxes of `head`. */
function moovOf(head: Uint8Array): Uint8Array | null {
  let pos = 0;
  while (pos + 8 <= head.length) {
    let size = u32(head, pos);
    const type = fourcc(head, pos + 4);
    if (size === 1) {
      // 64-bit size; a box that large is media, never the index.
      if (type === "moov") return null;
      break;
    }
    if (size === 0) size = head.length - pos;
    if (size < 8) return null;
    if (type === "moov") {
      return pos + size <= head.length ? head.subarray(pos, pos + size) : null;
    }
    pos += size;
  }
  return null;
}

/**
 * A fragmented MP4 announces itself with `mvex` inside `moov` (the movie
 * extends into fragments); its media then comes in `moof`/`mdat` pairs.
 */
export function isFragmentedMp4(head: Uint8Array): boolean {
  const moov = moovOf(head);
  if (moov) return find(moov, "mvex") >= 0;
  return find(head, "moof") >= 0;
}

/**
 * `video/mp4; codecs="…"` for the video track, or `null` when the file's
 * codec is neither H.264 nor HEVC. The HEVC string follows ISO/IEC
 * 14496-15 Annex E — the same `hvc1.2.4.L150.B0` WebKit derives itself.
 */
export function mp4VideoMime(head: Uint8Array): string | null {
  const moov = moovOf(head) ?? head;

  const avcC = find(moov, "avcC");
  if (avcC >= 0 && avcC + 8 <= moov.length) {
    const profile = moov[avcC + 5];
    const compat = moov[avcC + 6];
    const level = moov[avcC + 7];
    return `video/mp4; codecs="avc1.${hex2(profile)}${hex2(compat)}${hex2(level)}"`;
  }

  const hvcC = find(moov, "hvcC");
  if (hvcC >= 0 && hvcC + 4 + 13 <= moov.length) {
    const c = hvcC + 4; // start of the HEVCDecoderConfigurationRecord
    const entry =
      find(moov, "hev1") >= 0 && find(moov, "hvc1") < 0 ? "hev1" : "hvc1";
    const b1 = moov[c + 1];
    const space = ["", "A", "B", "C"][b1 >> 6];
    const tier = (b1 >> 5) & 1 ? "H" : "L";
    const profileIdc = b1 & 0x1f;
    // Compatibility flags are written bit-reversed.
    let flags = u32(moov, c + 2);
    let reversed = 0;
    for (let i = 0; i < 32; i++) {
      reversed = ((reversed << 1) | (flags & 1)) >>> 0;
      flags >>>= 1;
    }
    const constraints: number[] = [];
    for (let i = 0; i < 6; i++) constraints.push(moov[c + 6 + i]);
    while (
      constraints.length > 0 &&
      constraints[constraints.length - 1] === 0
    ) {
      constraints.pop();
    }
    const level = moov[c + 12];
    const tail = constraints
      .map((b) => `.${b.toString(16).toUpperCase()}`)
      .join("");
    return `video/mp4; codecs="${entry}.${space}${profileIdc}.${reversed.toString(16).toUpperCase()}.${tier}${level}${tail}"`;
  }

  return null;
}

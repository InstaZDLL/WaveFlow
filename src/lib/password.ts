/// Letters and digits a person can copy by eye onto another device: no
/// `0`/`O`, `1`/`l`/`I`. Nothing that needs quoting either — an MPD client
/// sends the password as a protocol argument, where a space or a quote
/// changes how the line is read.
const ALPHABET = "abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/**
 * A random password of `length` characters from {@link ALPHABET}, drawn
 * from the platform's CSPRNG. A byte that would bias the draw (one past
 * the largest multiple of the alphabet's size) is thrown away rather than
 * folded in with a modulo.
 */
export function generatePassword(length = 16): string {
  const limit = 256 - (256 % ALPHABET.length);
  const bytes = new Uint8Array(length * 2);
  let out = "";
  while (out.length < length) {
    crypto.getRandomValues(bytes);
    for (const byte of bytes) {
      if (byte >= limit) continue;
      out += ALPHABET[byte % ALPHABET.length];
      if (out.length === length) break;
    }
  }
  return out;
}

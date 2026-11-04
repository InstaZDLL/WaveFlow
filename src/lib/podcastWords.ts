import type { TFunction } from "i18next";

/** The words a podcast command or a channel's `last_error` may hold. A status
 *  (`http_404`) is handled apart, and an unknown word reads as a failure. */
const KNOWN_WORDS = new Set([
  "disabled",
  "offline",
  "profile_changed",
  "channel_gone",
  "episode_gone",
  "open_failed",
  "no_source",
  "not_seekable",
  "not_stored",
  "server_unreachable",
  "server_signed_out",
  "server_refused",
  "server_conflict",
  "destination_refused",
  "write_failed",
  "address_refused",
  "too_many_redirects",
  "too_large",
  "unreached",
  "invalid_feed",
  "not_audio",
  "quota_exceeded",
]);

/**
 * What to tell someone about a word from `podcastErrorWord` or `last_error`.
 * `null` is an error that carried no word: it is logged where it happened, and
 * its text is not shown, since nothing vouches for it.
 */
export function podcastFailureMessage(
  t: TFunction,
  word: string | null,
): string {
  if (word) {
    const status = /^http_(\d+)$/.exec(word);
    if (status) return t("podcasts.errors.http", { status: status[1] });
    if (KNOWN_WORDS.has(word)) return t(`podcasts.errors.${word}`);
  }
  return t("podcasts.errors.unknown");
}

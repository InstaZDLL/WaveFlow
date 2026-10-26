//! Lyrics fetch + cache.
//!
//! Lazy multi-tier lookup, in order:
//!   1. Local DB cache (`lyrics` table, keyed by `track_id`)
//!   2. Embedded lyrics tag inside the audio file (via lofty), preferring
//!      a synced `SYNCEDLYRICS` tag over the plain `USLT` / `LYRICS` ones
//!   3. Local sidecar file — `{stem}.ttml` / `{stem}.lrc` / `{stem}.txt`
//!      next to the audio file, or inside a `Lyrics/` (case-insensitive)
//!      subfolder next to it. The richest wins (synced, then word-timed,
//!      then the newest file, then `.ttml` > `.lrc` > `.txt`).
//!   4. The generic `description` field — last of the local tiers,
//!      because it is the only one that is a guess about a field meant
//!      for something else
//!   5. Musixmatch Enhanced LRC when word-level timing exists
//!   6. LRCLIB public API (matched by artist + track + album + duration)
//!   7. Query-based external providers before caching a network miss
//!
//! Whichever tier hits first becomes the cached entry. We never refetch
//! once a row exists — the user can manually overwrite by importing a
//! `.lrc` file via [`import_lrc_file`].
//!
//! One exception, and it is narrow: a cached `embedded` row that is a
//! distribution service's credit rather than lyrics is dropped on
//! read and re-resolved. Those rows were written by a bug in tier 4
//! (see below), and without this the fix would only have reached
//! tracks nobody had opened yet.
//!
//! Because of that cache-first rule, **only a confirmed negative may be
//! cached for good** (#391). A lookup where nobody answered is
//! `Unavailable`, not a miss: it persists nothing, so the next panel open
//! retries. Caching it would freeze a one-off network blip into a
//! permanent "no lyrics" that only a manual refetch could clear.
//!
//! Between the two sits the **partial miss** (#720): some providers
//! answered "nothing", others failed or were left out. It is cached with
//! a `retry_after` stamp — served as a miss until then, looked up again
//! after. It used to be treated as `Unavailable`, so a single provider
//! that was down for good (a host refusing connections, an endpoint that
//! wants a cookie) kept every lyrics-less track from ever being cached,
//! and each open replayed the whole chain.
//!
//! Which providers are asked, and for which tracks, is
//! [`super::lyrics_providers`]: the user's per-provider switches (#722),
//! the excluded genres (#721), and the cooldown of a failing provider.
//!
//! **Prefer-LRCLIB toggle** (`profile_setting['lyrics.prefer_lrclib']`,
//! issue #378): when on, [`fetch_lyrics`] flips tiers 2–4 (the local
//! ones) to run *after* the online providers — LRCLIB / Musixmatch /
//! the fallback chain win, and the local tiers become the fallback used
//! only when the network has nothing (so a track LRCLIB doesn't carry
//! still shows its own embedded lyrics rather than nothing). Default off
//! (local wins). Applies to on-demand fetch + refetch; the bulk
//! [`run_prefetch`] path stays local-first (a cheap gap-filler).

mod cache;
mod fetch;
mod format;
mod local;
mod model;
mod online;
mod plugin;
mod prefetch;
mod radio;
mod save;
mod settings;

use cache::*;
pub use fetch::*;
use format::*;
use local::*;
pub use model::*;
pub use online::*;
use plugin::*;
pub use prefetch::*;
pub use radio::*;
pub use save::*;
pub use settings::*;

#[cfg(test)]
mod tests;

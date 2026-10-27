//! Where the rebuildable caches live (issue #619).
//!
//! Reported by email: WaveFlow stores artwork on the system drive even
//! when it is itself installed on another one, and a `C:` that is
//! critically low on space takes the whole machine down with it.
//!
//! # What moves, and why that is the line
//!
//! Only the **evictable** directories move. Every one of them is
//! content-addressed or LRU-evicted, so the worst outcome of a failed
//! move, a missing drive or a half-copy is a re-fetch. The databases,
//! the manually chosen motion covers and Canvas clips, and the offline
//! downloads all stay in the app-data tree: recreating those means the
//! user redoing work by hand, so moving them would be a migration
//! rather than a setting — with a restart, a schema guard to satisfy,
//! and a failure mode that loses data. That is the second option the
//! issue offers, and it is deliberately not the one taken here.
//!
//! # The three hazards, and where each is handled
//!
//! - **A move must never destroy the only copy.** [`set_cache_location`]
//!   copies, then persists the new location, and leaves the removal to
//!   the next startup. An interruption at any point leaves a whole copy
//!   on disk and a setting that names a whole copy.
//! - **The asset protocol has a static scope.** `tauri.conf.json` only
//!   allows `$APPDATA/…` and `$APPLOCALDATA/…`, so artwork served from
//!   another drive would silently fail to load — no error, no console
//!   message, just an image that never appears. [`grant_asset_scope`]
//!   widens it at runtime, which means every startup: a scope grant
//!   lives in the process, not on disk.
//! - **`AppPaths` is captured once, at boot.** `AppState` hands out a
//!   plain `AppPaths` and 95 call sites read it directly, so a move
//!   cannot take effect in the running process: every write after it
//!   would still land at the old root, and a background task holding a
//!   clone could split one batch of files across both. So the move
//!   copies and persists, and the *process restarts* to adopt the new
//!   location — after which a startup pass removes the old copy, at a
//!   moment when nothing is reading it. Deleting it eagerly, before the
//!   restart, would point the still-running app at directories that no
//!   longer exist.
//! - **A removable or network drive can be missing at launch.**
//!   [`resolve_cache_root`] falls back to the default location for that
//!   session *without clearing the stored choice*, so plugging the drive
//!   back in and relaunching picks it up again. The frontend is told,
//!   because a silent fallback looks exactly like the caches having been
//!   wiped.

mod cleanup;
mod copy;
mod location;
mod relocated;
mod root;
mod sizes;

pub use cleanup::*;
use copy::*;
pub use location::*;
pub use relocated::*;
pub use root::*;
use sizes::*;

#[cfg(test)]
mod tests;

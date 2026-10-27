// Re-export `canonical_name` so existing call sites
// (`crate::commands::scan::canonical_name` in `commands/radio.rs` and
// `commands/similar.rs`) keep resolving after the helper moved to
// `waveflow_core::scanner` in step 6.d.
pub use waveflow_core::scanner::canonical_name;

mod artist_images;
mod extract;
mod folder;
mod progress;
mod walk;

pub use artist_images::*;
use extract::*;
pub use folder::*;
pub use progress::*;
use walk::*;

#[cfg(test)]
mod walk_tests;

#[cfg(test)]
mod counter_tests;

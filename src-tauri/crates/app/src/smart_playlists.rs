//! Thin re-export — the smart-playlist engine moved to
//! [`waveflow_core::smart_playlists`] in step 6.c of the Phase 1.a
//! refactor. Submodules are re-exported individually so existing
//! `crate::smart_playlists::{cover, custom, generator, on_repeat}::*`
//! paths keep resolving without churn.

pub use waveflow_core::smart_playlists::{
    cover, custom, generator, on_repeat, PathsContext, SmartPlaylistRules,
};

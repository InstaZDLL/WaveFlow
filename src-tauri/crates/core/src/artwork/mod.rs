//! Shared artwork helpers — the hash-addressed cache for remote
//! covers + the SIMD thumbnail pipeline that backs every list /
//! grid render. No Tauri, no DB; designed to plug straight into the
//! future `waveflow-server` axum handlers as well.

pub mod metadata;
pub mod motion_cache;
pub mod thumbnails;

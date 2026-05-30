//! Repository abstraction layer.
//!
//! Every trait here describes the persistence surface a single domain
//! entity exposes — what `list`, `get`, `upsert`, `delete` look like in
//! storage terms, without committing to a backend. The SQLite
//! implementations under [`sqlite`] back the desktop crate today; the
//! future `waveflow-server` (RFC-001 §6.5) will ship a parallel
//! `postgres` set under the same trait surface.
//!
//! All traits are `async`, declared with `#[async_trait::async_trait]`
//! so they stay dyn-compatible. Errors flow through
//! [`crate::error::CoreError`].

pub mod library;
pub mod playlist;
pub mod profile;
pub mod track;

// Backend implementations are gated behind their respective Cargo
// features so a consumer that only needs one of the two stays cheap
// to compile. The `sqlite` feature is what the desktop crate enables;
// `postgres` is for `waveflow-server` (RFC-001 §6.5). The trait
// modules above stay always-compiled — they describe the contract,
// not the storage.
#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(feature = "postgres")]
pub mod postgres;

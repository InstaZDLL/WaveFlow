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
pub mod sqlite;

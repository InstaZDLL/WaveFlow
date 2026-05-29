//! Postgres implementations of the repository traits.
//!
//! Each `PostgresXxxRepository` is a thin newtype over `sqlx::PgPool`,
//! cheap to construct (the pool is `Arc`-backed) and `Clone`. The
//! `waveflow-server` axum handlers build one per request:
//!
//! ```ignore
//! let repo = PostgresProfileRepository::new(state.db.clone());
//! repo.list_all().await
//! ```
//!
//! Migration policy mirrors the desktop's rule from `CLAUDE.md`:
//! once merged, a migration file is immutable. Schema evolution
//! creates a new dated migration (`YYYYMMDDhhmmss_<slug>.sql`).
//! `waveflow-server` owns the migration files themselves — this crate
//! only provides the trait implementations that assume the schema
//! exists.

pub mod profile;

pub use profile::PostgresProfileRepository;

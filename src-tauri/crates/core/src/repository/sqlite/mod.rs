//! SQLite implementations of the repository traits.
//!
//! Each `SqliteXxxRepository` is a thin newtype over `sqlx::SqlitePool`,
//! cheap to construct (the pool is `Arc`-backed) and `Clone`. Command
//! handlers in `crates/app` build one per call:
//!
//! ```ignore
//! let repo = SqliteProfileRepository::new(state.app_db.clone());
//! repo.list_all().await
//! ```

pub mod library;
pub mod playlist;
pub mod profile;

pub use library::SqliteLibraryRepository;
pub use playlist::SqlitePlaylistRepository;
pub use profile::SqliteProfileRepository;

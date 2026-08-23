use serde::{Serialize, Serializer};

/// Top-level error type for the Tauri backend.
///
/// Implements [`serde::Serialize`] so it can be returned from Tauri commands.
/// The wire format is a single `String` (the `Display` representation).
///
/// During the Phase 1.a refactor (RFC-001) this type lives at the
/// boundary: it wraps `waveflow_core::error::CoreError` for everything
/// portable (storage, IO, profile invariants) and carries the
/// Tauri-specific variants that have no place in a `waveflow-server`
/// build (`tauri::Error`, `MissingAppDataDir` from `dirs::data_dir()`,
/// the audio engine's `cpal`/`rubato` error wrappers). The legacy
/// generic variants (`Database`, `Io`, `ProfileNotFound`, …) are kept
/// here for now so existing call sites continue to compile; future
/// commits migrate them to `CoreError` as their owning modules move
/// into `crates/core`.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Errors raised by `waveflow-core` functions. Stays at the top of
    /// the enum so reviewers immediately see where new error sources
    /// land once the bulk of the migration completes.
    #[error(transparent)]
    Core(#[from] waveflow_core::error::CoreError),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    /// The database holds a migration this build doesn't ship — it was
    /// written by a newer version of WaveFlow. Split out of the generic
    /// `Migration` variant because it's the one migration failure with
    /// a remedy the user can act on, and startup turns it into a dialog
    /// instead of a panic (issue #526). Raised by
    /// [`crate::db::schema_guard::ensure_not_from_the_future`].
    #[error(
        "{} was written by a newer version of WaveFlow (database schema {version})",
        scope.label()
    )]
    SchemaFromTheFuture {
        /// Which database — decides what the user can actually do about
        /// it, so startup branches the remedy on it.
        scope: crate::db::schema_guard::DbScope,
        /// The highest applied migration this build doesn't know.
        version: i64,
        /// When that migration was applied, `YYYY-MM-DD HH:MM:SS` as
        /// SQLite recorded it. `None` if the column was NULL.
        installed_on: Option<String>,
    },

    /// A migration this build *does* ship was applied in a different
    /// form — same version, different SQL. The other way a downgrade
    /// (or a beta whose migration was amended before release) lands,
    /// and the one sqlx reports as `VersionMismatch`. Split out for the
    /// same reason as [`AppError::SchemaFromTheFuture`]: it has a
    /// remedy, and the alternative was a panic out of the Tauri `setup`
    /// hook. Raised by
    /// [`crate::db::schema_guard::ensure_no_foreign_migration`].
    #[error(
        "{} was written by a different build of WaveFlow (migration {version} does not match)",
        scope.label()
    )]
    SchemaWrittenElsewhere {
        /// Which database — decides what the user can do about it.
        scope: crate::db::schema_guard::DbScope,
        /// The first applied migration whose SQL differs from ours.
        version: i64,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("tauri error: {0}")]
    Tauri(#[from] tauri::Error),

    #[error("profile not found: id={0}")]
    ProfileNotFound(i64),

    // Used by upcoming library/scan/queue commands that require an active
    // profile. Referenced here so the variant stays in the public API.
    #[allow(dead_code)]
    #[error("no profile is currently active")]
    NoActiveProfile,

    /// A command was told which profile the caller believed was active
    /// and it isn't anymore. Raised by
    /// [`AppState::require_profile_pool_for`](crate::state::AppState::require_profile_pool_for)
    /// so a write queued before a `switch_profile` lands nowhere instead
    /// of in the new profile (issue #485). Callers treat it as a benign
    /// no-op — the UI re-reads the new profile's value anyway.
    #[error("active profile changed (expected id={expected}, active id={active:?})")]
    ProfileChanged { expected: i64, active: Option<i64> },

    #[error("app data directory is unavailable")]
    MissingAppDataDir,

    #[error("audio error: {0}")]
    Audio(String),

    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),

    /// Errors bubbled from the `waveflow-spotify` crate. Wraps the
    /// crate-local `SpotifyError` enum (network / DB / parse failures
    /// for the Spotify Web API client). Same `transparent` shape as
    /// `Core` — caller sees the original message without an extra
    /// "spotify error: " prefix.
    #[error(transparent)]
    Spotify(#[from] waveflow_spotify::SpotifyError),

    #[error("{0}")]
    Other(String),
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;

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

    #[error("app data directory is unavailable")]
    MissingAppDataDir,

    #[error("audio error: {0}")]
    Audio(String),

    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),

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

use serde::{Serialize, Serializer};

/// Top-level error type for the Tauri backend.
///
/// Implements [`serde::Serialize`] so it can be returned from Tauri commands.
/// The wire format is a single `String` (the `Display` representation).
#[derive(Debug, thiserror::Error)]
pub enum AppError {
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

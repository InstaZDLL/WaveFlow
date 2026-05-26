//! Generic error type shared by every `waveflow-core` function — and,
//! through a `#[from]` bridge in `crates/app/src/error.rs`, by the
//! Tauri command surface.
//!
//! Variants here are deliberately storage- and runtime-agnostic so
//! the same type can flow through axum handlers in the future
//! `waveflow-server`. Anything Tauri-specific (the command serialisation
//! bridge, `tauri::Error`, the `MissingAppDataDir` shape that depends on
//! `dirs::data_dir()`) lives in the app crate's `AppError`.

use thiserror::Error;

/// Top-level error type for `waveflow-core`.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("profile not found: id={0}")]
    ProfileNotFound(i64),

    #[error("no profile is currently active")]
    NoActiveProfile,

    #[error("{0}")]
    Other(String),
}

pub type CoreResult<T> = Result<T, CoreError>;

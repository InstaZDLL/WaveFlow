//! Crate-local error type. Mapped to the app crate's `AppError`
//! through `#[from] waveflow_spotify::SpotifyError` so call sites
//! keep using `?` without an explicit conversion.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpotifyError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    /// Network / HTTP / JSON parse / Spotify API failures + any
    /// caller-relevant string-flavoured error. The desktop surfaces
    /// these messages verbatim in toast / dialog UI, so phrasing is
    /// part of the contract — keep it user-facing French (or English
    /// when caller-friendly).
    #[error("{0}")]
    Other(String),
}

pub type SpotifyResult<T> = Result<T, SpotifyError>;

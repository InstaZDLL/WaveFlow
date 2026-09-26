use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Core(#[from] waveflow_core::error::CoreError),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("{} was written by a newer WaveFlow build (schema {version})", scope.label())]
    SchemaFromTheFuture {
        scope: crate::db::schema_guard::DbScope,
        version: i64,
        installed_on: Option<String>,
    },
    #[error("{} was written by a different WaveFlow build (migration {version})", scope.label())]
    SchemaWrittenElsewhere {
        scope: crate::db::schema_guard::DbScope,
        version: i64,
    },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("no profile is currently active")]
    NoActiveProfile,
    #[error("profile changed (expected {expected}, active {active:?})")]
    ProfileChanged { expected: i64, active: Option<i64> },
    #[error("app data directory is unavailable")]
    MissingAppDataDir,
    #[error("audio error: {0}")]
    Audio(String),
    #[error("{0}")]
    Other(String),
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}
pub type AppResult<T> = Result<T, AppError>;

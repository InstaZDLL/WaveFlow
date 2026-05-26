//! Domain DTOs — the shape of the data the WaveFlow clients see,
//! independent of how it's stored or transported.
//!
//! Every type here is `serde::Serialize`/`Deserialize` and (where it
//! mirrors a row in the SQLite schema) gains a `sqlx::FromRow` derive
//! when the `sqlite` feature is on. The current desktop crate enables
//! that feature; the future `waveflow-server` will enable a different
//! one (`postgres`) once it's implemented.

pub mod library;
pub mod playlist;
pub mod profile;
pub mod track;

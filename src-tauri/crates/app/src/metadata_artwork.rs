//! Thin re-export — the shared metadata-artwork cache moved to
//! [`waveflow_core::artwork::metadata`] in step 6.a of the Phase 1.a
//! refactor. Kept here so existing `crate::metadata_artwork::*` call
//! sites resolve without churn.

pub use waveflow_core::artwork::metadata::*;

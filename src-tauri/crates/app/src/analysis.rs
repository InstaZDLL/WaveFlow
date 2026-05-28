//! Thin re-export — the audio analysis pipeline moved to
//! [`waveflow_core::analysis`] in step 6.b of the Phase 1.a refactor.
//! Kept here so existing `crate::analysis::*` call sites resolve
//! without churn.

pub use waveflow_core::analysis::*;

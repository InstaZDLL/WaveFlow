//! Thin re-export — the real thumbnail pipeline moved to
//! [`waveflow_core::artwork::thumbnails`] in step 6.a of the Phase 1.a
//! refactor. Kept here so existing `crate::thumbnails::*` call sites
//! resolve without churn.

pub use waveflow_core::artwork::thumbnails::*;

//! Tauri commands for binding a profile to a remote server (RFC-005).
//!
//! Deliberately small: identifying a server, signing in, and the two
//! ways of undoing that. Everything the projection does sits behind the
//! binding these commands establish.
//!
//! ## Two ways out, because they are not the same intent
//!
//! [`remote_sign_out`] drops the credentials only. The binding and the
//! projection survive, so the cached remote library stays readable and
//! signing back into the same account resumes from its cursor.
//!
//! [`remote_forget_server`] drops everything, pending writes included.
//! That is the destructive one, and the UI must present it as such.

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

mod account;
mod activity;
mod catalogue;
mod play_queue;
mod projection;
mod stream;
mod transfer;

pub use account::*;
pub use activity::*;
pub use catalogue::*;
pub use play_queue::*;
pub use projection::*;
pub use stream::*;
pub use transfer::*;

/// The leased pool of the active profile, or `None` when there is no
/// active profile.
///
/// No profile is an ordinary state for these reads — onboarding queries
/// them before one exists — so it answers empty rather than failing.
///
/// Returns the *lease*, not a bare connection: the caller acquires its
/// connection from the returned pool and keeps the lease bound for the
/// whole read, so a profile switch can't close the pool under a live
/// connection (CLAUDE.md "profile-scoped pool").
async fn optional_pool(state: &AppState) -> AppResult<Option<crate::state::ProfilePool>> {
    match state.require_profile_pool().await {
        Ok(pool) => Ok(Some(pool)),
        Err(AppError::NoActiveProfile) => Ok(None),
        Err(err) => Err(err),
    }
}

//! What the interface is told about how it is being drawn (#595).
//!
//! A silent downgrade is its own kind of confusing: someone whose app
//! has quietly stopped using the GPU deserves to know that, and to have
//! a way back that does not involve finding a file. Both live here —
//! the reading, and the undo.
//!
//! The decision itself is made long before any of this, in
//! [`crate::render_mode`], because it has to happen before a window
//! exists.

use crate::error::{AppError, AppResult};
use crate::render_mode::{self, RenderDecision};

/// How this launch is drawing, and why.
///
/// `None` only when the decision never ran — no app-data directory, so
/// nothing could be armed or remembered. The interface shows nothing
/// rather than guessing at a mode it cannot know.
#[tauri::command]
pub fn renderer_status() -> Option<RenderDecision> {
    render_mode::current()
}

/// Forget the software fallback, so the next launch tries the GPU.
///
/// Takes effect on the next start and says nothing about this one:
/// the web engine read its environment when its process began, and
/// nothing can move it now. The interface is what tells the user that.
#[tauri::command]
pub fn renderer_retry_gpu() -> AppResult<()> {
    let Some(decision) = render_mode::current() else {
        return Err(AppError::Other(
            "the renderer state is not available this launch".into(),
        ));
    };
    if !decision.can_retry_gpu {
        // Nothing to undo. Refused rather than pretended, so a button
        // that cannot help does not report that it did.
        return Err(AppError::Other(
            "nothing to undo: this launch's renderer was not chosen from the stored state".into(),
        ));
    }
    render_mode::retry_gpu();
    Ok(())
}

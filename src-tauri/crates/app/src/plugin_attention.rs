//! A plugin telling the user something only they can fix.
//!
//! Plugins that sign in with a credential the user pasted (a cookie, a
//! token) can have it expire or be revoked. Until now that looked exactly
//! like "no result": the plugin logged a warning nobody reads, and the
//! Canvas or the lyrics simply stopped appearing.
//!
//! The contract is a prefix on the error string a plugin returns:
//! [`AUTH_REQUIRED_PREFIX`]. The host turns it into one toast, **once per
//! launch and per plugin** — the failure repeats on every track, the
//! notice must not. Every other error keeps its old meaning (logged,
//! skipped), and a host older than this contract logs the prefixed one
//! the same way, so a plugin can adopt it without dropping support for
//! older hosts.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use waveflow_core::plugin::runtime::SourceError;

/// Start of a plugin error that means "the credential you gave me was
/// refused; paste a new one".
pub const AUTH_REQUIRED_PREFIX: &str = "auth-required:";

const EVENT: &str = "plugin:attention";

static APP: OnceLock<AppHandle> = OnceLock::new();
static ANNOUNCED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AttentionPayload<'a> {
    plugin_id: &'a str,
    kind: &'static str,
}

/// Called once at startup, so a plugin path without an `AppHandle` in
/// reach (the lyrics waterfall) can still announce.
pub fn init(app: AppHandle) {
    let _ = APP.set(app);
}

/// Whether `err` is the plugin saying its credential was refused.
pub fn is_auth_required(err: &SourceError) -> bool {
    matches!(err, SourceError::Plugin(msg) if msg.starts_with(AUTH_REQUIRED_PREFIX))
}

/// Announce `err` to the user when it is an auth-required error, the first
/// time this plugin raises one in this launch. Returns whether it was one,
/// so the caller can skip its generic log line.
pub fn inspect(plugin_id: &str, err: &SourceError) -> bool {
    if !is_auth_required(err) {
        return false;
    }
    let first = {
        let Ok(mut guard) = ANNOUNCED.lock() else {
            return true;
        };
        guard
            .get_or_insert_with(HashSet::new)
            .insert(plugin_id.to_string())
    };
    if first {
        tracing::warn!(
            plugin_id,
            err = %err.detail(),
            "plugin credential refused; asking the user for a new one"
        );
        if let Some(app) = APP.get() {
            let _ = app.emit(
                EVENT,
                AttentionPayload {
                    plugin_id,
                    kind: "auth-required",
                },
            );
        }
    } else {
        tracing::debug!(plugin_id, "plugin credential still refused");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_prefixed_plugin_error_asks_for_a_credential() {
        assert!(is_auth_required(&SourceError::Plugin(
            "auth-required: sp_dc refused".into()
        )));
        assert!(!is_auth_required(&SourceError::Plugin(
            "network timeout".into()
        )));
        assert!(!is_auth_required(&SourceError::Trap(
            "auth-required: not from a trap".into()
        )));
    }
}

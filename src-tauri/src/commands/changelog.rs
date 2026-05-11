//! Read-only changelog endpoint backed by a build-time git log dump.
//!
//! `build.rs` runs `git log --pretty=…` once at compile time, parses the
//! conventional-commit header (`type(scope): subject`), and writes the
//! result to `$OUT_DIR/changelog.json`. We embed that JSON via
//! `include_str!` so the shipped binary carries its own changelog —
//! no git binary required at runtime.

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

const CHANGELOG_JSON: &str = include_str!(concat!(env!("OUT_DIR"), "/changelog.json"));

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangelogEntry {
    pub hash: String,
    /// Conventional-commit type — `feat`, `fix`, `chore`, etc.
    #[serde(rename = "type")]
    pub kind: String,
    pub scope: Option<String>,
    pub subject: String,
    pub breaking: bool,
    /// ISO-8601 committer date.
    pub date: String,
}

#[tauri::command]
pub async fn get_changelog() -> AppResult<Vec<ChangelogEntry>> {
    serde_json::from_str(CHANGELOG_JSON)
        .map_err(|e| AppError::Other(format!("decode embedded changelog: {e}")))
}

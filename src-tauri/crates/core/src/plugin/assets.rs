//! Sidecar asset resolver (option B from the plugin SDK design).
//!
//! Plugins bundle large read-only data files (the 10 MB SQLite
//! catalogue for Web Radio, lyric databases, language models) under
//! `assets/` next to `manifest.toml`. The wasm component never sees
//! the host filesystem directly — when a plugin calls
//! `waveflow:host/storage.read-asset(path)`, that call lands here.
//!
//! Defence-in-depth: the manifest already validates that asset
//! filenames don't contain `..`, but the host re-checks at every
//! read because a manifest is just a string the user dropped on
//! disk and we don't want a single missed validation to escape the
//! plugin directory.
//!
//! Asset SHA-256 verification (when declared in the manifest) is
//! also performed here; a verified mismatch returns an error and
//! the read does NOT succeed — drive-by tampering is detectable
//! without a full signing chain.

use std::fs;
use std::path::{Path, PathBuf};

use super::manifest::Manifest;

#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("asset io: {0}")]
    Io(#[from] std::io::Error),
    #[error("asset not declared in manifest: {0}")]
    NotDeclared(String),
    #[error("asset path tries to escape sandbox: {0}")]
    PathEscape(String),
    #[error("asset hash mismatch (expected {expected}, got {got})")]
    HashMismatch { expected: String, got: String },
}

/// Resolves [`waveflow:host/storage.read-asset(path)`](../wit/waveflow-host.wit)
/// calls against the plugin's bundled `assets/` directory.
///
/// Constructed once per plugin instantiation. Holds a snapshot of
/// the manifest's allow-list so the host can pin "what the user
/// installed" against "what the plugin is asking for" — a manifest
/// edited mid-session won't be re-read until the host reloads the
/// plugin.
pub struct AssetResolver {
    /// `<plugin-dir>/assets/` — root for every read.
    assets_root: PathBuf,
    /// Declared assets (filename → optional sha256). Reads against
    /// any other path return [`AssetError::NotDeclared`].
    declared: std::collections::HashMap<String, Option<String>>,
}

impl AssetResolver {
    /// Build a resolver from a parsed manifest + the plugin's
    /// on-disk root directory. The manifest's `[[assets]]` table
    /// drives the allow-list; the directory is the prefix we
    /// resolve against.
    pub fn new(plugin_dir: &Path, manifest: &Manifest) -> Self {
        let declared = manifest
            .assets
            .iter()
            .map(|a| (a.filename.clone(), a.sha256.clone()))
            .collect();
        Self {
            assets_root: plugin_dir.join("assets"),
            declared,
        }
    }

    /// Read one asset. `path` is what the plugin passed to the
    /// host import — relative to `assets/`, no leading `/`, no
    /// `..` segments.
    pub fn read(&self, path: &str) -> Result<Vec<u8>, AssetError> {
        let expected_hash = self
            .declared
            .get(path)
            .ok_or_else(|| AssetError::NotDeclared(path.into()))?
            .clone();

        // Re-check `..` even though the manifest validator catches
        // it — defence in depth. Reject anything other than
        // forward path components, no `..`, no absolute root.
        if path.is_empty() {
            return Err(AssetError::PathEscape(path.into()));
        }
        let candidate = self.assets_root.join(path);
        // Canonicalise both sides so a symlink under `assets/`
        // can't escape — `canonicalize` resolves the link AND
        // ensures the target exists.
        let canon_root = self
            .assets_root
            .canonicalize()
            .unwrap_or_else(|_| self.assets_root.clone());
        let canon_candidate = candidate.canonicalize()?;
        if !canon_candidate.starts_with(&canon_root) {
            return Err(AssetError::PathEscape(path.into()));
        }

        let bytes = fs::read(&canon_candidate)?;

        if let Some(expected) = expected_hash {
            let got = sha256_hex(&bytes);
            if !constant_time_eq(expected.as_bytes(), got.as_bytes()) {
                return Err(AssetError::HashMismatch { expected, got });
            }
        }

        Ok(bytes)
    }
}

/// Lower-case hex SHA-256. Pulled in via the existing `blake3`
/// dependency tree's transitive `sha2` would be nice but we already
/// take `sha2` directly through `metadata::lastfm`'s signing, so
/// reuse it.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Constant-time slice equality so a malicious sidecar can't be
/// timing-probed for its expected hash.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut acc = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

#[cfg(test)]
mod tests;

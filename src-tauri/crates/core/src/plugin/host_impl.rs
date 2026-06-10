//! Host import implementations.
//!
//! Backs every method on `waveflow:host/{http,log,storage}` with a
//! concrete impl on [`HostCtx`](crate::plugin::runtime::HostCtx).
//! Permission decisions happen here — the manifest's
//! `[permissions]` table is captured into [`HostPermissions`] at
//! instantiation time and consulted on every call. A denied call
//! returns the WIT `result<_, string>` `Err` variant; nothing traps
//! the guest, the plugin is expected to surface the error
//! gracefully (e.g. "permission denied: <url>").
//!
//! Why permissions are pinned at instantiate-time + not re-read
//! from the manifest on every call: the manifest is a string on
//! disk a sideload or update can swap. Pinning means a sideloaded
//! manifest swap mid-session can't sneak new permissions in without
//! the host explicitly reloading the plugin.

use std::fs;
use std::path::PathBuf;

use globset::{Glob, GlobSet, GlobSetBuilder};
use sha2::{Digest, Sha256};

use crate::plugin::bindings::source::waveflow::host as wit_host;
use crate::plugin::manifest::Manifest;
use crate::plugin::runtime::HostCtx;

/// Default scratch-store quota — 10 MB per plugin (sum of all keys).
/// Mirrors the contract spelled in `waveflow-host.wit`. Phase 3 lets
/// the host pick a different value per plugin; Phase 2b uses one
/// number across the board.
pub const STATE_QUOTA_BYTES: usize = 10 * 1024 * 1024;

/// Snapshot of plugin permissions taken at instantiation. Holds the
/// HTTP allowlist as a compiled [`GlobSet`] so per-request matching
/// is O(1) once the matcher is built. The storage flags are
/// single bools — there's no per-key permission system, every
/// granted permission applies to the whole per-plugin scope.
pub struct HostPermissions {
    /// Compiled allowlist. `None` = no HTTP allowed (manifest didn't
    /// list any pattern). Empty `Vec<String>` in the manifest is
    /// equivalent to "no HTTP" — we don't ship a deny-by-empty
    /// vs deny-by-omission distinction.
    http: Option<GlobSet>,

    /// `true` if `waveflow:host/storage.read-asset` is allowed.
    pub storage_read: bool,

    /// `true` if `waveflow:host/storage.{read,write}-state` is
    /// allowed. Single toggle gates both sides — see the WIT file
    /// for the rationale (a plugin that can write but not read is
    /// the same as one that can write into a key it then re-reads).
    pub storage_state: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid http allowlist pattern {pattern:?}: {source}")]
    InvalidPattern {
        pattern: String,
        #[source]
        source: globset::Error,
    },
    #[error("storage quota exceeded: would use {would_use} bytes (cap {cap})")]
    QuotaExceeded { would_use: usize, cap: usize },
    #[error("state key contains illegal character: {0:?}")]
    InvalidStateKey(String),
}

impl HostPermissions {
    /// Compile the manifest's `[permissions]` table into a runtime
    /// snapshot. Each glob pattern is compiled once here; the cost
    /// is paid at load time rather than on every HTTP request.
    pub fn from_manifest(manifest: &Manifest) -> Result<Self, HostError> {
        let http = if manifest.permissions.http.is_empty() {
            None
        } else {
            let mut builder = GlobSetBuilder::new();
            for pattern in &manifest.permissions.http {
                let glob = Glob::new(pattern).map_err(|source| HostError::InvalidPattern {
                    pattern: pattern.clone(),
                    source,
                })?;
                builder.add(glob);
            }
            let set = builder.build().map_err(|source| HostError::InvalidPattern {
                pattern: "<set>".into(),
                source,
            })?;
            Some(set)
        };
        Ok(Self {
            http,
            storage_read: manifest.permissions.storage_read,
            storage_state: manifest.permissions.storage_state,
        })
    }

    /// Empty permissions — denies everything. Useful for tests and
    /// for the `HostCtx::stub()` fallback when a host wants to
    /// build a context without committing to a manifest yet.
    pub fn empty() -> Self {
        Self {
            http: None,
            storage_read: false,
            storage_state: false,
        }
    }

    /// `true` iff the URL matches at least one allowlist pattern.
    /// Globs use shell-style wildcards: `*` matches any path
    /// segment, `**` matches multiple. Manifest authors typically
    /// write `https://api.example.com/*` to allow any path under a
    /// fixed host.
    pub fn http_allowed(&self, url: &str) -> bool {
        match &self.http {
            None => false,
            Some(set) => set.is_match(url),
        }
    }
}

/// Filesystem-backed scratch key/value store for
/// `waveflow:host/storage.{read,write}-state`. One file per key,
/// filename = SHA-256 hex of the key (so an arbitrary UTF-8 key
/// becomes a safe filename on every OS). Total bytes across files
/// is capped at [`StateStore::quota_bytes`]; writes that would
/// overflow return [`HostError::QuotaExceeded`] without touching
/// the on-disk state.
pub struct StateStore {
    /// `<app-data>/waveflow/plugin-data/<plugin-id>/` — one tree
    /// per plugin. The host creates the directory lazily on first
    /// write so a plugin that never calls `write-state` doesn't
    /// leave an empty dir behind.
    dir: PathBuf,
    /// Cap on the sum of all file sizes under `dir`. Honoured at
    /// `write` time only — the host doesn't shrink the store
    /// retroactively if quota lands lower than current usage.
    quota_bytes: usize,
}

impl StateStore {
    pub fn new(dir: PathBuf, quota_bytes: usize) -> Self {
        Self { dir, quota_bytes }
    }

    /// SHA-256 hex of the key, used as the on-disk filename. Keeps
    /// arbitrary UTF-8 keys (incl. ones with slashes / nulls / spaces)
    /// safe on every filesystem without forcing the plugin author
    /// to pre-escape.
    fn key_path(&self, key: &str) -> PathBuf {
        let digest = Sha256::digest(key.as_bytes());
        let mut name = String::with_capacity(digest.len() * 2);
        for b in digest {
            name.push_str(&format!("{b:02x}"));
        }
        self.dir.join(name)
    }

    /// Validate a state key. We accept anything except embedded NUL
    /// (which trips OS-level file APIs on every platform) — the
    /// SHA-256 filename strategy handles slashes and control
    /// characters safely, but a NUL in the input string itself is
    /// a code-smell from a plugin and worth refusing loudly.
    fn check_key(key: &str) -> Result<(), HostError> {
        if key.is_empty() || key.contains('\0') {
            return Err(HostError::InvalidStateKey(key.into()));
        }
        Ok(())
    }

    /// Read a key. Missing keys return `Ok(None)` — distinguishing
    /// "never set" from "explicitly empty" matters for plugins
    /// that store boolean flags by presence.
    pub fn read(&self, key: &str) -> Result<Option<Vec<u8>>, HostError> {
        Self::check_key(key)?;
        let path = self.key_path(key);
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(HostError::Io(e)),
        }
    }

    /// Write a key. Creates the per-plugin directory on first call.
    /// Quota check sums every existing file (skipping the one
    /// being overwritten so an in-place write of unchanged size is
    /// never quota-rejected) and bails before touching the FS if
    /// the new total would exceed [`Self::quota_bytes`].
    pub fn write(&self, key: &str, value: &[u8]) -> Result<(), HostError> {
        Self::check_key(key)?;
        fs::create_dir_all(&self.dir)?;
        let path = self.key_path(key);

        let mut existing_total = 0usize;
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let entry_path = entry.path();
            // Skip the file we're about to overwrite — its existing
            // size doesn't count against the quota for the new write
            // (otherwise an in-place same-size update would always
            // fail at usage ~= quota).
            if entry_path == path {
                continue;
            }
            // file_type() skips the directory entry itself and any
            // future subdirs — only count regular files.
            if entry.file_type()?.is_file() {
                existing_total = existing_total.saturating_add(entry.metadata()?.len() as usize);
            }
        }
        let projected = existing_total.saturating_add(value.len());
        if projected > self.quota_bytes {
            return Err(HostError::QuotaExceeded {
                would_use: projected,
                cap: self.quota_bytes,
            });
        }

        fs::write(&path, value)?;
        Ok(())
    }

    /// Test-only: report current on-disk usage. Walks the store
    /// directory; not cheap. Public for the integration tests in
    /// `runtime::tests` that verify quota accounting.
    #[cfg(test)]
    pub fn used_bytes(&self) -> Result<usize, HostError> {
        if !self.dir.exists() {
            return Ok(0);
        }
        let mut total = 0usize;
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                total = total.saturating_add(entry.metadata()?.len() as usize);
            }
        }
        Ok(total)
    }
}

// ----- waveflow:host/http -------------------------------------------------

impl wit_host::http::Host for HostCtx {
    fn send(
        &mut self,
        req: wit_host::http::Request,
    ) -> wasmtime::Result<Result<wit_host::http::Response, String>> {
        if !self.permissions.http_allowed(&req.url) {
            return Ok(Err(format!("permission denied: {}", req.url)));
        }
        // Offline mode short-circuit. Returns an empty 503 body so
        // plugins can keep their normal HTTP error handling path
        // (status + retry-later) instead of needing a separate
        // "offline" branch. Matches the empty-payload convention
        // CLAUDE.md spells for every other outbound HTTP path
        // (Deezer, Last.fm, LRCLIB).
        if (self.offline_probe)() {
            return Ok(Ok(wit_host::http::Response {
                status: 503,
                headers: Vec::new(),
                body: Vec::new(),
            }));
        }
        let method = match req.method.parse::<reqwest::Method>() {
            Ok(m) => m,
            Err(_) => return Ok(Err(format!("invalid http method: {}", req.method))),
        };
        let mut builder = self.http_client.request(method, &req.url);
        for (k, v) in &req.headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        if let Some(body) = req.body {
            builder = builder.body(body);
        }
        let resp = match builder.send() {
            Ok(r) => r,
            Err(e) => return Ok(Err(e.to_string())),
        };
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    v.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let body = match resp.bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Ok(Err(e.to_string())),
        };
        Ok(Ok(wit_host::http::Response {
            status,
            headers,
            body,
        }))
    }
}

// ----- waveflow:host/log --------------------------------------------------

impl wit_host::log::Host for HostCtx {
    fn emit(&mut self, level: wit_host::log::Level, message: String) -> wasmtime::Result<()> {
        let plugin = self.plugin_id.as_str();
        // tracing's level macros are compile-time selected; one
        // match arm per level is the only shape that lets the
        // host's subscriber filter on the right severity.
        match level {
            wit_host::log::Level::Trace => tracing::trace!(plugin, "{}", message),
            wit_host::log::Level::Debug => tracing::debug!(plugin, "{}", message),
            wit_host::log::Level::Info => tracing::info!(plugin, "{}", message),
            wit_host::log::Level::Warn => tracing::warn!(plugin, "{}", message),
            wit_host::log::Level::Error => tracing::error!(plugin, "{}", message),
        }
        Ok(())
    }
}

// ----- waveflow:host/storage ----------------------------------------------

impl wit_host::storage::Host for HostCtx {
    fn read_asset(&mut self, path: String) -> wasmtime::Result<Result<Vec<u8>, String>> {
        if !self.permissions.storage_read {
            return Ok(Err("permission denied: storage.read".into()));
        }
        let Some(assets) = &self.assets else {
            return Ok(Err("plugin has no bundled assets".into()));
        };
        match assets.read(&path) {
            Ok(b) => Ok(Ok(b)),
            Err(e) => Ok(Err(e.to_string())),
        }
    }

    fn read_state(&mut self, key: String) -> wasmtime::Result<Result<Option<Vec<u8>>, String>> {
        if !self.permissions.storage_state {
            return Ok(Err("permission denied: storage.state".into()));
        }
        match self.state.read(&key) {
            Ok(v) => Ok(Ok(v)),
            Err(e) => Ok(Err(e.to_string())),
        }
    }

    fn write_state(
        &mut self,
        key: String,
        value: Vec<u8>,
    ) -> wasmtime::Result<Result<(), String>> {
        if !self.permissions.storage_state {
            return Ok(Err("permission denied: storage.state".into()));
        }
        match self.state.write(&key, &value) {
            Ok(()) => Ok(Ok(())),
            Err(e) => Ok(Err(e.to_string())),
        }
    }
}

/// Register every `waveflow:host/*` import on the given linker.
/// Called once by [`crate::plugin::runtime::PluginRuntime::build_linker`].
/// Adding a new host interface = one new line here.
///
/// The `HasSelf<HostCtx>` marker tells wasmtime's `HasData`-based
/// `add_to_linker` to resolve the Host trait against `HostCtx`
/// itself (D::Data<'a> = &'a mut HostCtx), so the closure is just
/// `|ctx| ctx`. Wasmtime 45 made the D generic explicit so embedders
/// CAN split the import state across multiple types; we don't need
/// that flexibility and `HasSelf` is the canonical "everything lives
/// on the Store data" knob.
pub fn add_to_linker(
    linker: &mut wasmtime::component::Linker<HostCtx>,
) -> wasmtime::Result<()> {
    use wasmtime::component::HasSelf;
    wit_host::http::add_to_linker::<_, HasSelf<HostCtx>>(linker, |ctx| ctx)?;
    wit_host::log::add_to_linker::<_, HasSelf<HostCtx>>(linker, |ctx| ctx)?;
    wit_host::storage::add_to_linker::<_, HasSelf<HostCtx>>(linker, |ctx| ctx)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::manifest::{PluginMetadata, Permissions};

    fn manifest_with_permissions(permissions: Permissions) -> Manifest {
        Manifest {
            schema_version: 1,
            plugin: PluginMetadata {
                id: "test".into(),
                name: "Test".into(),
                version: "1.0.0".into(),
                author: "x".into(),
                world: waveflow_plugin_sdk::worlds::SOURCE_V1.into(),
                description: None,
                homepage: None,
                license: None,
            },
            permissions,
            assets: vec![],
        }
    }

    #[test]
    fn no_http_pattern_means_deny() {
        let m = manifest_with_permissions(Permissions::default());
        let p = HostPermissions::from_manifest(&m).expect("compile empty allowlist");
        assert!(!p.http_allowed("https://example.com/api"));
    }

    #[test]
    fn http_allowlist_matches_glob_pattern() {
        let m = manifest_with_permissions(Permissions {
            http: vec!["https://*.radio-browser.info/**".into()],
            storage_read: false,
            storage_state: false,
        });
        let p = HostPermissions::from_manifest(&m).expect("compile glob");
        assert!(p.http_allowed("https://de1.api.radio-browser.info/json/stations"));
        assert!(p.http_allowed("https://us.api.radio-browser.info/json/stations/byuuid/abc"));
        assert!(!p.http_allowed("https://malicious.example.com/api"));
        assert!(!p.http_allowed("http://de1.api.radio-browser.info/json/stations"));
    }

    #[test]
    fn invalid_glob_pattern_surfaces_error() {
        let m = manifest_with_permissions(Permissions {
            http: vec!["[".into()],
            storage_read: false,
            storage_state: false,
        });
        match HostPermissions::from_manifest(&m) {
            Ok(_) => panic!("expected glob compile error, got Ok"),
            Err(HostError::InvalidPattern { pattern, .. }) => assert_eq!(pattern, "["),
            Err(err) => panic!("expected InvalidPattern, got {err:?}"),
        }
    }

    #[test]
    fn state_store_roundtrips_value() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = StateStore::new(tmp.path().join("plugin-x"), STATE_QUOTA_BYTES);
        assert!(store.read("missing").expect("read").is_none());
        store.write("favourites", b"a;b;c").expect("write");
        let got = store.read("favourites").expect("read after write");
        assert_eq!(got.as_deref(), Some(b"a;b;c".as_slice()));
    }

    #[test]
    fn state_store_rejects_quota_overflow() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = StateStore::new(tmp.path().join("plugin-x"), 1024);
        store
            .write("a", &vec![0u8; 600])
            .expect("first write fits");
        match store.write("b", &vec![0u8; 600]) {
            Ok(()) => panic!("expected quota error"),
            Err(HostError::QuotaExceeded { would_use, cap }) => {
                assert_eq!(would_use, 1200);
                assert_eq!(cap, 1024);
            }
            Err(err) => panic!("expected QuotaExceeded, got {err:?}"),
        }
        // The rejected write must NOT have left a partial file.
        assert!(store.read("b").expect("read").is_none());
    }

    #[test]
    fn state_store_overwrite_does_not_double_count() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = StateStore::new(tmp.path().join("plugin-x"), 1024);
        store
            .write("one", &vec![0u8; 800])
            .expect("first write fits");
        // Overwriting `one` with another 800-byte payload should
        // leave usage at 800, not 1600 — the quota check must
        // exclude the file being replaced.
        store
            .write("one", &vec![1u8; 800])
            .expect("overwrite fits");
        assert_eq!(store.used_bytes().expect("usage"), 800);
    }

    #[test]
    fn http_send_short_circuits_when_offline() {
        // Build a HostCtx with an HTTP allowlist that authorises
        // example.com + an offline probe that says "yes, offline".
        // The send must NOT hit the network — it must return an
        // empty 503 response synthesised by the offline branch,
        // verifiable by `Vec::is_empty()` on the body without
        // depending on what `example.com` would actually serve.
        use crate::plugin::runtime::HostCtx;
        use crate::plugin::bindings::source::waveflow::host::http::{Host, Request};
        use std::sync::Arc;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = HostCtx::stub(64 * 1024 * 1024, tmp.path().to_path_buf());
        // Allow the request URL so we DON'T trip the permission
        // branch — the offline check is what we want to exercise.
        ctx.permissions = HostPermissions::from_manifest(&manifest_with_permissions(
            crate::plugin::manifest::Permissions {
                http: vec!["https://example.com/*".into()],
                storage_read: false,
                storage_state: false,
            },
        ))
        .expect("compile allowlist");
        ctx.offline_probe = Arc::new(|| true);

        let req = Request {
            method: "GET".into(),
            url: "https://example.com/api".into(),
            headers: Vec::new(),
            body: None,
        };
        let result = ctx.send(req).expect("host call should not trap");
        match result {
            Ok(resp) => {
                assert_eq!(resp.status, 503);
                assert!(resp.headers.is_empty());
                assert!(resp.body.is_empty());
            }
            Err(err) => panic!("expected empty 503 response, got Err({err})"),
        }
    }

    #[test]
    fn state_store_rejects_nul_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = StateStore::new(tmp.path().join("plugin-x"), STATE_QUOTA_BYTES);
        match store.read("bad\0key") {
            Ok(_) => panic!("expected InvalidStateKey"),
            Err(HostError::InvalidStateKey(_)) => {}
            Err(err) => panic!("expected InvalidStateKey, got {err:?}"),
        }
        match store.write("bad\0key", b"x") {
            Ok(()) => panic!("expected InvalidStateKey"),
            Err(HostError::InvalidStateKey(_)) => {}
            Err(err) => panic!("expected InvalidStateKey, got {err:?}"),
        }
    }
}

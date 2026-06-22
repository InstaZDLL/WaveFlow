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
use std::io::Read;
use std::path::{Path, PathBuf};

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

/// Hard cap on the response body size `waveflow:host/http.send`
/// will buffer for a guest. A misbehaving (or malicious)
/// allowlisted server could otherwise stream multi-GB into our
/// `Vec<u8>` before the guest ever sees it — the wasmtime epoch
/// would eventually trap the call but only after the host
/// allocated the entire response. 10 MB covers the largest
/// realistic JSON catalogue (radio-browser station lists clock
/// around 1-3 MB) with a 3× safety margin; anything bigger is
/// surfaced as a clean `Err("response body too large")` the plugin
/// can pivot on (paginate, retry with `Range`, surface to the user).
const MAX_BODY_BYTES: u64 = 10 * 1024 * 1024;

/// Sentinel filename for the per-plugin write lock. The dot prefix
/// keeps it inconspicuous when a user inspects the scratch dir, and
/// the underscore prefix on the extension keeps it out of any
/// "looks like a key" pattern. Excluded from the quota tally + from
/// the read-dir iteration in [`StateStore::write`].
const LOCK_FILE: &str = ".write-lock";

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
    ///
    /// Concurrency contract — the function holds an exclusive lock
    /// on `LOCK_FILE` for the duration of the quota check + the
    /// rename. Two concurrent writers (same OR different processes
    /// — the lock is OS-level via [`fs::File::lock`]) serialise on
    /// it, so the quota check + the file replacement act as one
    /// atomic transaction. Without the lock, two writers could each
    /// observe the pre-write total + each pass the check + each
    /// write, leaving the store over quota.
    ///
    /// Atomic replace — values are first written to a sibling
    /// `<hash>.tmp` file, then `fs::rename`d onto the canonical
    /// path. Both POSIX `rename` and Windows `MoveFileEx` are
    /// atomic when source + destination live in the same directory
    /// on the same filesystem, so a concurrent reader (or a host
    /// crash mid-write) can only see either the old content or
    /// the complete new content — never a truncated partial.
    pub fn write(&self, key: &str, value: &[u8]) -> Result<(), HostError> {
        Self::check_key(key)?;
        fs::create_dir_all(&self.dir)?;
        let path = self.key_path(key);
        let lock_path = self.dir.join(LOCK_FILE);

        // Acquire the per-plugin write lock. The handle drops at
        // end-of-function, which releases the OS lock — explicit
        // `unlock()` not needed.
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        // `File::lock` is stable since Rust 1.89; clippy infers a lower MSRV
        // from the edition since the workspace declares no `rust-version`.
        #[allow(clippy::incompatible_msrv)]
        lock_file.lock()?;

        let existing_total = sum_state_dir_bytes(&self.dir, Some(&path), &lock_path)?;
        let projected = existing_total.saturating_add(value.len());
        if projected > self.quota_bytes {
            return Err(HostError::QuotaExceeded {
                would_use: projected,
                cap: self.quota_bytes,
            });
        }

        // Write to a temp sibling first. `with_extension("tmp")`
        // gives a deterministic per-key tmp path; since the lock
        // serialises writes against `path`, two writers can never
        // contend on the same tmp name.
        let tmp_path = path.with_extension("tmp");
        fs::write(&tmp_path, value)?;
        // `fs::rename` overwrites the destination on Windows
        // (MoveFileEx with REPLACE_EXISTING) and POSIX (atomic
        // unlink + link), so this single call swaps the old
        // content for the new without an intermediate "no file"
        // window. If the rename fails the tmp file gets left
        // behind; the next successful write of the same key
        // overwrites the same tmp path, so the leak is bounded
        // by the number of distinct keys ever written.
        fs::rename(&tmp_path, &path)?;
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
        let lock_path = self.dir.join(LOCK_FILE);
        sum_state_dir_bytes(&self.dir, None, &lock_path)
    }
}

/// Sum the size (in bytes) of every regular file under `dir`,
/// skipping `lock_path` (the per-plugin write lock — outside the
/// quota) and `skip` (typically the file being overwritten, whose
/// existing size doesn't count against the projected total — an
/// in-place same-size write would otherwise fail at usage ≈ quota).
///
/// `.tmp` siblings are also excluded: `StateStore::write` stages a
/// `<hash>.tmp` next to the canonical file before `fs::rename`, and
/// a host crash between the two steps leaves an orphan that the
/// next write of the same key overwrites. Counting orphans toward
/// the quota would inflate usage by every interrupted write the
/// store ever survived — that's a real footgun on a long-lived
/// install — so we skip the suffix at the read step.
fn sum_state_dir_bytes(
    dir: &Path,
    skip: Option<&Path>,
    lock_path: &Path,
) -> Result<usize, HostError> {
    let mut total = 0usize;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let entry_path = entry.path();
        if entry_path == *lock_path {
            continue;
        }
        if skip == Some(entry_path.as_path()) {
            continue;
        }
        if entry_path.extension().and_then(|e| e.to_str()) == Some("tmp") {
            continue;
        }
        if entry.file_type()?.is_file() {
            total = total.saturating_add(entry.metadata()?.len() as usize);
        }
    }
    Ok(total)
}

// ----- waveflow:host/http -------------------------------------------------

impl wit_host::http::Host for HostCtx {
    fn send(
        &mut self,
        req: wit_host::http::Request,
    ) -> wasmtime::Result<Result<wit_host::http::Response, String>> {
        // ORDERING INVARIANT — DO NOT REORDER WITHOUT READING THIS.
        //
        // 1. Offline short-circuit FIRST. CLAUDE.md is explicit:
        //    "every outbound HTTP path (Deezer, Last.fm, similar,
        //    LRCLIB) checks `offline::is_offline()` first and
        //    short-circuits to an empty payload or cache. Treat
        //    new HTTP code paths the same way." Plugin HTTP is a
        //    new HTTP code path, so it behaves like the rest of
        //    the workspace.
        // 2. Permission allowlist SECOND. The WIT contract in
        //    `host.wit` says "validates `url` against the allowlist
        //    BEFORE dispatching". It pins permission ahead of the
        //    NETWORK dispatch — it does NOT pin it ahead of host-
        //    state short-circuits like offline mode. Both gates run
        //    before any actual `builder.send()`, so the WIT
        //    invariant is honoured either way.
        // 3. Network dispatch LAST.
        //
        // The information-leak concern raised in earlier review
        // rounds — a guest probing offline state via 503 vs
        // permission-denied — doesn't bite: the guest already
        // ships its own manifest, so it always knows its own
        // allowlist; the only thing it could learn from the
        // probe is the host's offline flag, which is user-visible
        // anyway.
        if (self.offline_probe)() {
            return Ok(Ok(wit_host::http::Response {
                status: 503,
                headers: Vec::new(),
                body: Vec::new(),
            }));
        }
        if !self.permissions.http_allowed(&req.url) {
            return Ok(Err(format!("permission denied: {}", req.url)));
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
        // Bounded read: `take(MAX + 1)` lets us detect a body that
        // ran over the cap without ever holding more than `MAX + 1`
        // bytes in memory. The `+ 1` is the saturating count — if
        // the read returns exactly that we know there was at least
        // one more byte the take refused. `resp.bytes()` would have
        // happily slurped a multi-GB stream into a `Vec<u8>` before
        // we got a chance to reject it.
        let mut body = Vec::new();
        let read = match resp.take(MAX_BODY_BYTES + 1).read_to_end(&mut body) {
            Ok(n) => n as u64,
            Err(e) => return Ok(Err(e.to_string())),
        };
        if read > MAX_BODY_BYTES {
            return Ok(Err("response body too large".to_string()));
        }
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
    fn http_send_offline_supersedes_permission_denial() {
        // Locks the ORDERING INVARIANT in `Host::send`. CLAUDE.md
        // says every outbound HTTP path checks offline first; a
        // guest probing a non-allowlisted URL while the host is
        // offline must see the same empty 503 every other call
        // gets. The WIT contract pins permission ahead of the
        // network dispatch, not ahead of host-state short-circuits
        // — both gates still fire before any actual `builder.send()`.
        use crate::plugin::runtime::HostCtx;
        use crate::plugin::bindings::source::waveflow::host::http::{Host, Request};
        use std::sync::Arc;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = HostCtx::stub(64 * 1024 * 1024, tmp.path().to_path_buf());
        // Empty allowlist (= deny everything) and offline.
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
                assert_eq!(
                    resp.status, 503,
                    "offline must short-circuit before the allowlist check"
                );
            }
            Err(err) => panic!("expected empty 503 response, got Err({err})"),
        }
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
    fn state_store_lock_file_excluded_from_quota() {
        // After a write, the on-disk dir contains both the value
        // file AND the `.write-lock` sentinel. The lock file must
        // not count against the quota or every subsequent write
        // would slowly run out of room over the lock-file's own
        // size (zero today but defensive — Windows occasionally
        // leaves a few bytes in similar handles).
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = StateStore::new(tmp.path().join("plugin-x"), 1024);
        store.write("a", &vec![0u8; 1024]).expect("fills exactly");
        assert_eq!(store.used_bytes().expect("usage"), 1024);
        // Lock file is present on disk but excluded from the tally.
        assert!(tmp.path().join("plugin-x").join(".write-lock").exists());
    }

    #[test]
    fn state_store_ignores_orphan_tmp_files() {
        // Simulates a host crash between `fs::write(tmp)` and
        // `fs::rename(tmp, path)`: the orphan `<hash>.tmp` sits in
        // the dir but must not consume the quota. Otherwise long-
        // lived installs would slowly lose available state to every
        // interrupted write that ever happened.
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("plugin-x");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let store = StateStore::new(dir.clone(), 1024);

        // Synthesise an orphan tmp (any filename ending in `.tmp`).
        std::fs::write(dir.join("deadbeef.tmp"), vec![0u8; 900]).expect("write orphan");
        // Quota tally must still be zero — the orphan is invisible.
        assert_eq!(store.used_bytes().expect("usage"), 0);

        // A 1 KiB write must succeed; the orphan does NOT count
        // against the cap.
        store.write("a", &vec![1u8; 1024]).expect("fills quota");
        assert_eq!(store.used_bytes().expect("usage"), 1024);
    }

    #[test]
    fn state_store_serialises_concurrent_writes() {
        // Two threads racing the same key. With the lock + atomic
        // rename, both writes succeed and the post-condition is
        // ONE of the two values — never a truncated mix. Without
        // the serialisation, the quota check + write could
        // interleave and leave the store over quota.
        use std::sync::Arc as StdArc;
        use std::thread;

        let tmp = tempfile::tempdir().expect("tempdir");
        let store = StdArc::new(StateStore::new(tmp.path().join("plugin-x"), 8192));

        let store_a = StdArc::clone(&store);
        let store_b = StdArc::clone(&store);
        let h_a = thread::spawn(move || {
            for _ in 0..10 {
                store_a.write("shared", &vec![b'A'; 1000]).expect("write A");
            }
        });
        let h_b = thread::spawn(move || {
            for _ in 0..10 {
                store_b.write("shared", &vec![b'B'; 1000]).expect("write B");
            }
        });
        h_a.join().expect("thread A");
        h_b.join().expect("thread B");

        // Whichever thread wrote last wins. The post-condition we
        // verify is that the result is one of the two pure values —
        // never a mix from a torn write.
        let got = store.read("shared").expect("read").expect("present");
        assert!(got.len() == 1000);
        let is_pure_a = got.iter().all(|&b| b == b'A');
        let is_pure_b = got.iter().all(|&b| b == b'B');
        assert!(
            is_pure_a || is_pure_b,
            "concurrent write produced a torn value"
        );
        // Quota was 8192 — 10 writes per side × 1000 bytes serially
        // unsupervised would balloon to 20 000 bytes. With the lock,
        // the dir holds exactly one 1000-byte file at any time.
        assert_eq!(store.used_bytes().expect("usage"), 1000);
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

//! Plugin store (Phase 2) — install + update plugins from the curated
//! registry (`InstaZDLL/waveflow-plugins`).
//!
//! The runtime + sideload/enable/uninstall surface already lives in
//! [`super::plugins`]; this module adds the missing *distribution* layer:
//! fetch the remote catalogue, then download + hash-verify + unpack a
//! plugin into the writable sideload root so the runtime can hot-load it.
//!
//! **Integrity model.** A registry entry pins `version` + the blake3 of
//! `plugin.wasm`. We download the release asset from the entry's repo but
//! trust the REGISTRY, not the release — a blake3 mismatch is refused. So
//! a compromised release can't push a payload the app will load, and a
//! takedown (removing the registry entry) stops new installs everywhere.
//!
//! Every outbound fetch honours [`offline::is_offline`] like every other
//! HTTP path in the workspace.

use std::io::{Cursor, Read, Seek};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;
use waveflow_core::plugin::is_bundled_plugin;
use waveflow_core::plugin::manifest::Manifest;
use waveflow_core::plugin::PluginPaths;

use crate::error::{AppError, AppResult};
use crate::offline;
use crate::state::AppState;

/// Ordered registry sources. The app-controlled endpoint is primary so
/// the catalogue backend can evolve (region filtering, analytics, a DB)
/// without an app release; the raw-GitHub + jsDelivr URLs are serverless
/// fallbacks that keep the store working if the endpoint is down or not
/// yet deployed.
const REGISTRY_URLS: &[&str] = &[
    "https://waveflow.app/api/plugins/registry",
    "https://raw.githubusercontent.com/InstaZDLL/waveflow-plugins/main/registry.json",
    "https://cdn.jsdelivr.net/gh/InstaZDLL/waveflow-plugins@main/registry.json",
];

/// Registry schema version this build understands. A registry declaring
/// anything else is refused rather than mis-parsed.
const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Hard cap on a downloaded release asset. The runtime already caps the
/// unpacked `plugin.wasm` at 50 MB; the deflated zip (wasm + manifest +
/// optional assets) is always smaller, so 64 MB is generous headroom
/// that still refuses a hostile multi-GB body before it exhausts memory.
const MAX_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// Mirror of the runtime's `MAX_WASM_SIZE` (that const is private to
/// `core`). Refuse an unpacked `plugin.wasm` bigger than this before it
/// ever reaches the sideload root — the runtime would reject it at load
/// anyway, but failing here keeps a hostile release off disk entirely.
const MAX_WASM_SIZE: u64 = 50 * 1024 * 1024;

/// Cap on the manifest.toml read. A manifest is a handful of TOML lines;
/// anything near this is corrupt or hostile. Bounds the `read_zip_file`
/// allocation so a lying zip header can't force a large buffer.
const MAX_MANIFEST_SIZE: u64 = 256 * 1024;

// ----- registry wire types -------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct RegistryPermissions {
    #[serde(default)]
    http: Vec<String>,
    #[serde(default)]
    storage_read: bool,
    #[serde(default)]
    storage_state: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct RegistryEntry {
    id: String,
    name: String,
    description: String,
    author: String,
    repo: String,
    #[serde(default)]
    homepage: Option<String>,
    world: String,
    version: String,
    blake3: String,
    #[serde(default)]
    asset: Option<String>,
    permissions: RegistryPermissions,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    official: bool,
    #[serde(default)]
    min_app_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Registry {
    schema_version: u32,
    #[serde(default)]
    plugins: Vec<RegistryEntry>,
}

/// One store row for the frontend: the registry fields the UI renders,
/// flattened, plus the install/update/compat state resolved against what's
/// on disk and this build's version.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub repo: String,
    pub homepage: Option<String>,
    pub world: String,
    pub version: String,
    /// Allowlisted outbound hosts — shown before install so the user sees
    /// exactly what the plugin can reach (and it's enforced at runtime).
    pub http: Vec<String>,
    pub storage_read: bool,
    pub storage_state: bool,
    pub tags: Vec<String>,
    pub official: bool,
    /// Present on disk (sideloaded/installed) already.
    pub installed: bool,
    /// Version currently installed, `None` when not installed.
    pub installed_version: Option<String>,
    /// Registry version differs from the installed one → offer update.
    pub update_available: bool,
    /// This build satisfies the entry's `min_app_version`.
    pub compatible: bool,
}

// ----- helpers -------------------------------------------------------------

/// Fetch + parse the registry, trying each source in order. Honours
/// offline mode. A schema version we don't understand is a hard error
/// (the fallbacks mirror the same file, so retrying can't help).
async fn fetch_registry() -> AppResult<Registry> {
    if offline::is_offline() {
        return Err(AppError::Other(
            "offline mode is on; the plugin store is unavailable".into(),
        ));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::Other(format!("http client: {e}")))?;

    let mut last_err = String::from("no plugin-registry source reachable");
    for url in REGISTRY_URLS {
        match client.get(*url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<Registry>().await {
                Ok(reg) if reg.schema_version == SUPPORTED_SCHEMA_VERSION => return Ok(reg),
                Ok(reg) => {
                    return Err(AppError::Other(format!(
                        "unsupported plugin-registry schema_version {} (this build supports {SUPPORTED_SCHEMA_VERSION}); update WaveFlow",
                        reg.schema_version
                    )));
                }
                Err(e) => last_err = format!("{url}: decode failed: {e}"),
            },
            Ok(resp) => last_err = format!("{url}: HTTP {}", resp.status()),
            Err(e) => last_err = format!("{url}: {e}"),
        }
    }
    Err(AppError::Other(last_err))
}

/// Parse `MAJOR.MINOR.PATCH` (ignoring any pre-release suffix) into a
/// comparable tuple. Malformed input degrades to `(0,0,0)` so a garbage
/// `min_app_version` never wrongly blocks an install.
fn parse_semver(v: &str) -> (u32, u32, u32) {
    let core = v.split('-').next().unwrap_or(v);
    let mut it = core.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}

fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn is_compatible(min_app_version: Option<&str>) -> bool {
    match min_app_version {
        None => true,
        Some(min) => parse_semver(app_version()) >= parse_semver(min),
    }
}

/// Read the installed version for `id` by parsing its on-disk manifest.
/// `None` when not installed or the manifest id doesn't match the dir.
fn installed_version(paths: &PluginPaths, id: &str) -> Option<String> {
    let manifest_path = paths.manifest_path(id).ok()?;
    let manifest = Manifest::load_from_path(&manifest_path).ok()?;
    (manifest.plugin.id == id).then_some(manifest.plugin.version)
}

/// Read one named member out of a zip archive, bounded at `max_bytes`.
/// `Ok(None)` when the member is absent so the caller can distinguish
/// "missing" from "error". The read is `take`-capped and the capacity hint
/// is clamped, so a lying header or a decompression bomb can't force a
/// large allocation or materialise an oversized payload before the size
/// check — the member is rejected as it streams past the cap.
fn read_zip_file<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
    max_bytes: u64,
) -> AppResult<Option<Vec<u8>>> {
    match archive.by_name(name) {
        Ok(f) => {
            let cap = f.size().min(max_bytes) as usize;
            let mut buf = Vec::with_capacity(cap);
            let read = f.take(max_bytes + 1).read_to_end(&mut buf)? as u64;
            if read > max_bytes {
                return Err(AppError::Other(format!(
                    "{name} exceeds {max_bytes} bytes — refusing"
                )));
            }
            Ok(Some(buf))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(AppError::Other(format!("read {name} from plugin zip: {e}"))),
    }
}

/// Verify + unpack a downloaded plugin zip into the sideload root. Runs on
/// a blocking thread (inflate + fs ops). Order matters: hash + manifest
/// gates fire BEFORE anything touches disk, then a stage-then-swap keeps a
/// crashed install from leaving a half-written plugin dir.
fn install_from_zip_bytes(
    paths: &PluginPaths,
    plugin_id: &str,
    zip_bytes: &[u8],
    expected_blake3: &str,
    expected_version: &str,
    expected_world: &str,
) -> AppResult<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|e| AppError::Other(format!("open plugin zip: {e}")))?;

    // `read_zip_file` bounds each member at its cap (plugin.wasm at
    // MAX_WASM_SIZE, manifest at MAX_MANIFEST_SIZE) so an oversized member
    // is refused as it streams, never fully buffered first.
    let wasm = read_zip_file(&mut archive, "plugin.wasm", MAX_WASM_SIZE)?
        .ok_or_else(|| AppError::Other("plugin zip missing plugin.wasm".into()))?;
    let manifest_bytes = read_zip_file(&mut archive, "manifest.toml", MAX_MANIFEST_SIZE)?
        .ok_or_else(|| AppError::Other("plugin zip missing manifest.toml".into()))?;

    // 1. Hash gate — the registry, not the release, is the trusted pin.
    let actual = blake3::hash(&wasm).to_hex().to_string();
    if actual != expected_blake3 {
        return Err(AppError::Other(format!(
            "blake3 mismatch for {plugin_id}: registry pins {expected_blake3}, download is {actual} — refusing"
        )));
    }

    // 2. Manifest sanity — id/version/world must match what we asked for,
    //    so a mislabelled release can't masquerade as another plugin.
    let manifest_str = std::str::from_utf8(&manifest_bytes)
        .map_err(|e| AppError::Other(format!("manifest.toml is not utf-8: {e}")))?;
    let manifest = Manifest::parse(manifest_str)
        .map_err(|e| AppError::Other(format!("parse manifest: {e}")))?;
    if manifest.plugin.id != plugin_id {
        return Err(AppError::Other(format!(
            "manifest id {:?} does not match requested {plugin_id}",
            manifest.plugin.id
        )));
    }
    if manifest.plugin.version != expected_version {
        return Err(AppError::Other(format!(
            "manifest version {:?} does not match registry {expected_version}",
            manifest.plugin.version
        )));
    }
    if manifest.plugin.world != expected_world {
        return Err(AppError::Other(format!(
            "manifest world {:?} does not match registry {expected_world}",
            manifest.plugin.world
        )));
    }

    // 3. Stage into a temp dir under the sideload root, then swap.
    let install_dir = paths
        .plugin_dir(plugin_id)
        .map_err(|e| AppError::Other(format!("invalid plugin id: {e}")))?;
    let staging = paths.plugins_root.join(format!(".staging-{plugin_id}"));
    let _ = std::fs::remove_dir_all(&staging); // clear any crashed-install leftover
    std::fs::create_dir_all(&staging)?;

    std::fs::write(staging.join("manifest.toml"), &manifest_bytes)?;
    std::fs::write(staging.join("plugin.wasm"), &wasm)?;

    // Optional assets/ tree — zip-slip guarded via `enclosed_name`, and
    // bounded cumulatively so a decompression bomb in assets/ can't fill
    // the disk. Each file is `take`-capped by the remaining budget, and the
    // running total is checked after every copy.
    let mut assets_total: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| AppError::Other(format!("read plugin zip entry {i}: {e}")))?;
        if entry.is_dir() {
            continue;
        }
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let name = rel.to_string_lossy().replace('\\', "/");
        if let Some(sub) = name.strip_prefix("assets/") {
            if sub.is_empty() {
                continue;
            }
            let dest = staging.join("assets").join(sub);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let remaining = MAX_DOWNLOAD_BYTES.saturating_sub(assets_total);
            let mut out = std::fs::File::create(&dest)?;
            let written = std::io::copy(&mut (&mut entry).take(remaining + 1), &mut out)?;
            assets_total += written;
            if assets_total > MAX_DOWNLOAD_BYTES {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(AppError::Other(format!(
                    "plugin assets exceed {MAX_DOWNLOAD_BYTES} bytes — refusing"
                )));
            }
        }
    }

    // Swap: remove the old install, move staging into place. Windows can't
    // rename over an existing dir, so remove first. A crash between remove
    // and rename leaves the plugin absent (not corrupt); the next install
    // re-stages cleanly.
    match std::fs::remove_dir_all(&install_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(AppError::Io(e)),
    }
    if let Some(parent) = install_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&staging, &install_dir).map_err(|e| {
        let _ = std::fs::remove_dir_all(&staging);
        AppError::Io(e)
    })?;
    Ok(())
}

// ----- commands ------------------------------------------------------------

/// Fetch the curated catalogue and resolve each entry against what's
/// installed locally + this build's version, for the in-app store list.
#[tauri::command]
pub async fn list_plugin_marketplace(
    state: State<'_, AppState>,
) -> AppResult<Vec<MarketplaceEntry>> {
    let registry = fetch_registry().await?;
    let paths = state.paths.plugin_paths();

    let entries = tokio::task::spawn_blocking(move || {
        registry
            .plugins
            .into_iter()
            .map(|e| {
                let inst = installed_version(&paths, &e.id);
                let update_available = matches!(&inst, Some(v) if *v != e.version);
                MarketplaceEntry {
                    compatible: is_compatible(e.min_app_version.as_deref()),
                    update_available,
                    installed: inst.is_some(),
                    installed_version: inst,
                    id: e.id,
                    name: e.name,
                    description: e.description,
                    author: e.author,
                    repo: e.repo,
                    homepage: e.homepage,
                    world: e.world,
                    version: e.version,
                    http: e.permissions.http,
                    storage_read: e.permissions.storage_read,
                    storage_state: e.permissions.storage_state,
                    tags: e.tags,
                    official: e.official,
                }
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))?;

    Ok(entries)
}

/// Install (or update — same path, overwrites) a plugin by id. Downloads
/// the registry-pinned release asset, verifies its blake3 against the
/// registry, sanity-checks the manifest, and stage-swaps it into the
/// sideload root for the runtime to hot-load. The frontend calls this for
/// both a fresh install and an update; the on-disk swap is idempotent.
#[tauri::command]
pub async fn install_plugin_from_registry(
    state: State<'_, AppState>,
    plugin_id: String,
) -> AppResult<()> {
    super::plugins::validate_plugin_id_chars(&plugin_id)?;

    if offline::is_offline() {
        return Err(AppError::Other(
            "offline mode is on; can't install plugins".into(),
        ));
    }
    // A bundled id is owned by the installer and re-seeded at boot; a store
    // copy in the sideload root would be shadowed by the bundled one and
    // never actually load. Refuse rather than silently no-op.
    if is_bundled_plugin(&plugin_id) {
        return Err(AppError::Other(format!(
            "{plugin_id} ships with WaveFlow and can't be installed from the store"
        )));
    }

    let registry = fetch_registry().await?;
    let entry = registry
        .plugins
        .into_iter()
        .find(|e| e.id == plugin_id)
        .ok_or_else(|| AppError::Other(format!("plugin not in registry: {plugin_id}")))?;

    if !is_compatible(entry.min_app_version.as_deref()) {
        return Err(AppError::Other(format!(
            "{} requires WaveFlow {} or newer (you have {})",
            entry.name,
            entry.min_app_version.as_deref().unwrap_or("?"),
            app_version()
        )));
    }

    // The release asset lives in the entry's own repo (releases/download).
    let asset = entry
        .asset
        .clone()
        .unwrap_or_else(|| format!("{}-v{}.zip", entry.id, entry.version));
    let url = format!(
        "https://github.com/{}/releases/download/v{}/{}",
        entry.repo, entry.version, asset
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| AppError::Other(format!("http client: {e}")))?;
    let mut resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Other(format!("download {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(format!(
            "download {url}: HTTP {}",
            resp.status()
        )));
    }
    // content-length is an early-reject optimisation only — it can be
    // absent or lie, so the real bound is enforced while streaming below.
    if let Some(len) = resp.content_length() {
        if len > MAX_DOWNLOAD_BYTES {
            return Err(AppError::Other(format!(
                "release asset too large: {len} bytes (max {MAX_DOWNLOAD_BYTES})"
            )));
        }
    }
    // Stream the body, capping cumulative size so an absent/lying
    // content-length can't force an oversized buffer into memory.
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| AppError::Other(format!("read release body: {e}")))?
    {
        if bytes.len() as u64 + chunk.len() as u64 > MAX_DOWNLOAD_BYTES {
            return Err(AppError::Other(format!(
                "release asset exceeds {MAX_DOWNLOAD_BYTES} bytes — refusing"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }

    let paths = state.paths.plugin_paths();
    let expected_blake3 = entry.blake3.to_lowercase();
    let expected_version = entry.version.clone();
    let expected_world = entry.world.clone();
    let id_for_blocking = plugin_id.clone();

    // Serialise against enable/uninstall for this id (shared lock map).
    let _guard = super::plugins::lock_plugin(&state, &plugin_id).await;

    tokio::task::spawn_blocking(move || -> AppResult<()> {
        install_from_zip_bytes(
            &paths,
            &id_for_blocking,
            &bytes,
            &expected_blake3,
            &expected_version,
            &expected_world,
        )
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))??;

    tracing::info!(plugin_id, version = %entry.version, "plugin installed from registry");
    Ok(())
}

//! What the frontend is told about an installed plugin: its manifest,
//! permissions and assets, and why it failed to load.

use std::fs;
use std::io::Read;
use std::path::Path;

use serde::Serialize;
use waveflow_core::plugin::binfmt::{classify_wasm_binary, WasmBinaryKind};
use waveflow_core::plugin::manifest::{LocalizedString, Manifest};
use waveflow_core::plugin::runtime::{LoadFailure, RuntimeError};
use waveflow_core::plugin::PluginPaths;

use waveflow_core::plugin::is_bundled_plugin;

/// Frontend-facing summary of one installed plugin. Mirrors the
/// manifest's public-ish fields + carries the per-plugin enabled
/// flag from `app_setting`. We don't echo `schema_version` to the
/// frontend — it's an internal protocol detail the UI has no use
/// for.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub world: String,
    /// Plain string or `{ lang -> text }` — the frontend resolves it
    /// against the active i18next language (see `LocalizedString`).
    pub description: Option<LocalizedString>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    /// The world functions the manifest says it answers; empty = all of
    /// them (see `PluginMetadata::provides`).
    pub provides: Vec<String>,
    pub permissions: PluginPermissionsInfo,
    pub assets: Vec<PluginAssetInfo>,
    /// `true` when the host should instantiate this plugin. Driven
    /// by `app_setting['plugin.<id>.enabled']`; missing key = on
    /// (an installed plugin is enabled by default).
    pub enabled: bool,
    /// `true` when this plugin is shipped inside the installer and
    /// re-seeded at every boot. The UI replaces "Uninstall" with a
    /// disabled hint on bundled rows, and the backend refuses
    /// [`uninstall_plugin`] for the same id so the FS-remove + boot
    /// reseed cycle can't masquerade as an uninstall that "comes
    /// back" on next launch.
    pub bundled: bool,
    /// `true` when the manifest declares any `[[options]]` — the UI shows
    /// the ⚙️ gear + loads the options panel on demand for these.
    pub has_options: bool,
    /// `Some` when this plugin cannot run. Until this existed, a plugin
    /// that failed to load was more silent than one that was not
    /// installed at all: the row looked identical to a working one, the
    /// only trace was a `warn!` in a log file nobody opens, and the
    /// feature simply did nothing. See [`plugin_failure`].
    pub failure: Option<PluginFailure>,
}

/// Why a plugin will not run, in the two parts the interface needs.
///
/// `code` is a stable token the UI maps onto its own wording;
/// `detail` is the technical line printed underneath and is
/// deliberately NOT translated — it is what gets pasted into a bug
/// report, and a message the plugin's author can search for is worth
/// more than one in the reader's language.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginFailure {
    pub code: String,
    pub detail: String,
}

impl From<&RuntimeError> for PluginFailure {
    fn from(err: &RuntimeError) -> Self {
        Self {
            code: err.code().to_string(),
            detail: err.detail(),
        }
    }
}

impl From<LoadFailure> for PluginFailure {
    fn from(failure: LoadFailure) -> Self {
        Self {
            code: failure.code.to_string(),
            detail: failure.detail,
        }
    }
}

/// Read the first bytes of a plugin's `plugin.wasm` and report a file
/// the host could never load.
///
/// This exists because the runtime's recorded failure only appears
/// once something has actually asked the plugin to run. Someone who
/// opens Settings straight after launch would otherwise see a row that
/// looks perfectly healthy — which is precisely the situation this
/// whole change is about. Eight bytes settle the commonest case (a
/// core module where a component was meant) without paying for the
/// Cranelift compile a real load costs.
///
/// A `plugin.wasm` that cannot be read at all is reported too: a
/// plugin whose `manifest.toml` parsed but whose binary is missing is
/// broken in the same invisible way.
pub(super) fn probe_wasm_format(paths: &PluginPaths, plugin_id: &str) -> Option<PluginFailure> {
    let path = paths.wasm_path(plugin_id).ok()?;
    let err = match read_wasm_preamble(&path) {
        Ok(head) => match classify_wasm_binary(&head) {
            WasmBinaryKind::Component => return None,
            kind => RuntimeError::NotAComponent { kind },
        },
        Err(err) => RuntimeError::Io(err),
    };
    Some(PluginFailure::from(&err))
}

/// Up to eight bytes — fewer when the file is shorter, which the
/// classifier already answers correctly rather than erroring on.
fn read_wasm_preamble(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(8);
    fs::File::open(path)?.take(8).read_to_end(&mut head)?;
    Ok(head)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginPermissionsInfo {
    pub http: Vec<String>,
    pub storage_read: bool,
    pub storage_state: bool,
    /// `ui`-world redacted artist read — surfaced as its own "Can
    /// read: your artists" chip in the plugin's permission list.
    pub library_read_artists: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginAssetInfo {
    pub filename: String,
    pub description: Option<String>,
}

pub(super) fn manifest_to_info(
    manifest: Manifest,
    enabled: bool,
    failure: Option<PluginFailure>,
) -> PluginInfo {
    let bundled = is_bundled_plugin(&manifest.plugin.id);
    let has_options = !manifest.options.is_empty();
    PluginInfo {
        id: manifest.plugin.id,
        name: manifest.plugin.name,
        version: manifest.plugin.version,
        author: manifest.plugin.author,
        world: manifest.plugin.world,
        description: manifest.plugin.description,
        homepage: manifest.plugin.homepage,
        license: manifest.plugin.license,
        provides: manifest.plugin.provides,
        permissions: PluginPermissionsInfo {
            http: manifest.permissions.http,
            storage_read: manifest.permissions.storage_read,
            storage_state: manifest.permissions.storage_state,
            library_read_artists: manifest.permissions.library_read_artists,
        },
        assets: manifest
            .assets
            .into_iter()
            .map(|a| PluginAssetInfo {
                filename: a.filename,
                description: a.description,
            })
            .collect(),
        enabled,
        bundled,
        has_options,
        failure,
    }
}

/// Key shape for the per-plugin enabled toggle in `app_setting`.
/// One row per installed plugin — uninstall removes the row.
pub(super) fn enabled_key(plugin_id: &str) -> String {
    format!("plugin.{plugin_id}.enabled")
}

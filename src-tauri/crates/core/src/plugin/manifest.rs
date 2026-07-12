//! `manifest.toml` parser + validator.
//!
//! The plugin manifest is the contract between the author and the
//! host. The host parses it at install time AND every boot — the
//! second pass catches sideloads that were swapped after install
//! (a user dropping a different plugin into the directory, an
//! updater landing a new manifest).
//!
//! Validation rules:
//!
//! - `schema_version` MUST equal [`waveflow_plugin_sdk::MANIFEST_SCHEMA_VERSION`].
//!   A mismatch is a hard error — silently accepting a future
//!   schema would let unfamiliar fields go ignored.
//! - `world` MUST be a label [`waveflow_plugin_sdk::worlds::is_known`]
//!   recognises. Unknown world = the host can't safely bind the
//!   wasm component and refuses to load.
//! - Every permission in `permissions.kind` MUST be recognised by
//!   [`waveflow_plugin_sdk::permissions::is_known`]. Unknown
//!   permissions are rejected so a future-permission plugin
//!   doesn't silently get NO access.
//! - HTTP allowlist patterns are stored verbatim; the runtime
//!   matches them at request time. We don't pre-compile globs here
//!   because Phase 1 ships without `wasmtime` and avoiding a
//!   pattern-matching dep keeps the bundle slim.

use std::path::Path;

use serde::{Deserialize, Serialize};

use waveflow_plugin_sdk::{permissions, worlds, MANIFEST_SCHEMA_VERSION};

/// Parsed manifest. Public so commands / Tauri handlers can return
/// it verbatim to the frontend without redefining the shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// MUST equal [`MANIFEST_SCHEMA_VERSION`].
    pub schema_version: u32,
    pub plugin: PluginMetadata,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub assets: Vec<AssetDecl>,
    /// User-configurable options declared as `[[options]]` tables. Surfaced
    /// in the app's per-plugin settings; the chosen values reach the guest
    /// through `waveflow:host/config.get-option`.
    #[serde(default)]
    pub options: Vec<OptionDecl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMetadata {
    /// Plugin id — used as the directory name + the host scope for
    /// log events, storage keys, and HTTP allowlist matching.
    /// Restricted to `[a-z0-9-]+` so it's safe on every filesystem.
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    /// One of the labels in [`worlds`].
    pub world: String,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Permissions {
    /// HTTP allowlist patterns (e.g. `"https://radio-browser.info/*"`).
    /// Empty list means HTTP is denied. The host validates the
    /// request URL against this list at every `waveflow:host/http.send`
    /// invocation.
    #[serde(default)]
    pub http: Vec<String>,
    /// Whether the plugin can read its bundled sidecar assets
    /// (the read-only `assets/` directory shipped next to
    /// `manifest.toml`). Default `false`. Defended at the host
    /// import layer (`waveflow:host/storage.read-asset`).
    #[serde(default)]
    pub storage_read: bool,
    /// Whether the plugin can read AND write its per-user scratch
    /// store (`waveflow:host/storage.{read,write}-state`). One
    /// toggle covers both directions because the two host
    /// functions operate on the same per-plugin key/value space:
    /// granting only one would let a plugin write data it can
    /// never read back, or vice-versa, which isn't a meaningful
    /// security boundary. Subject to a 10 MB quota.
    #[serde(default)]
    pub storage_state: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetDecl {
    /// Path relative to `assets/`. `..` segments are rejected at
    /// load time so a malformed manifest can't escape the sandbox.
    pub filename: String,
    pub description: Option<String>,
    /// Optional SHA-256 of the file's contents, lower-case
    /// hex-encoded. When present the host verifies the asset
    /// before each load — makes drive-by tampering detectable
    /// without a full signing chain. The validator normalises
    /// uppercase input to lowercase so the comparison in
    /// [`crate::plugin::assets::AssetResolver`] is a simple
    /// constant-time byte equality.
    pub sha256: Option<String>,
}

/// Control types a `[[options]]` entry can declare. The value is always
/// stored + transported as a string; the plugin parses it per this type.
pub mod option_types {
    pub const BOOL: &str = "bool";
    pub const ENUM: &str = "enum";
    pub const TEXT: &str = "text";
    pub const ALL: &[&str] = &[BOOL, ENUM, TEXT];
}

/// One user-configurable option declared in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionDecl {
    /// Stable key the plugin reads via `config.get-option`. `[a-z0-9_-]+`.
    pub key: String,
    /// Control type — one of [`option_types::ALL`].
    #[serde(rename = "type")]
    pub option_type: String,
    /// Human-readable label for the settings control.
    pub label: String,
    /// Default value in string form (`"true"`/`"false"` for bool, one of
    /// `choices` for enum). `None` = no default (control starts empty/off).
    #[serde(default)]
    pub default: Option<String>,
    /// Allowed values — required + only meaningful for `type = "enum"`.
    #[serde(default)]
    pub choices: Vec<String>,
    /// Optional hint rendered under the control.
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest io: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest parse: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("manifest schema_version mismatch: got {got}, host supports {expected}")]
    SchemaVersionMismatch { got: u32, expected: u32 },
    #[error("manifest world unknown: {0}")]
    UnknownWorld(String),
    #[error("manifest permission unknown: {0}")]
    UnknownPermission(String),
    #[error("manifest plugin.id is empty")]
    EmptyId,
    #[error("manifest plugin.id contains illegal character: {0}")]
    InvalidIdChar(char),
    #[error("manifest asset filename contains '..': {0}")]
    AssetEscape(String),
    #[error("manifest asset filename is empty")]
    EmptyAssetFilename,
    #[error("manifest asset sha256 must be 64 hex chars: {0:?}")]
    InvalidAssetHash(String),
    #[error("manifest option key is empty")]
    EmptyOptionKey,
    #[error("manifest option key {0:?} has invalid chars (allowed: a-z 0-9 _ -)")]
    InvalidOptionKey(String),
    #[error("manifest option {0:?} has unknown type {1:?}")]
    UnknownOptionType(String, String),
    #[error("manifest enum option {0:?} declares no choices")]
    EnumOptionWithoutChoices(String),
}

impl Manifest {
    /// Parse `manifest.toml` from disk and run all the validation
    /// checks. Returns an [`Err`] on the first failure so a partial
    /// manifest never lands in caller-side state.
    pub fn load_from_path(path: &Path) -> Result<Self, ManifestError> {
        let raw = std::fs::read_to_string(path)?;
        Self::parse(&raw)
    }

    /// Parse + validate from raw TOML. Split out from
    /// [`Self::load_from_path`] so tests can feed strings without
    /// touching the filesystem.
    pub fn parse(raw: &str) -> Result<Self, ManifestError> {
        let mut parsed: Self = toml::from_str(raw)?;
        parsed.validate()?;
        // Lower-case every asset hash post-validation so the
        // downstream `AssetResolver::read` byte-equality compare
        // doesn't need to know the input came from a mixed-case
        // source. Validation already proved each hash is 64 hex
        // chars so this is just a case fold.
        for asset in &mut parsed.assets {
            if let Some(hash) = &mut asset.sha256 {
                hash.make_ascii_lowercase();
            }
        }
        Ok(parsed)
    }

    /// Run all the validation rules described in the module docs.
    fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(ManifestError::SchemaVersionMismatch {
                got: self.schema_version,
                expected: MANIFEST_SCHEMA_VERSION,
            });
        }

        if self.plugin.id.is_empty() {
            return Err(ManifestError::EmptyId);
        }
        for ch in self.plugin.id.chars() {
            let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-';
            if !ok {
                return Err(ManifestError::InvalidIdChar(ch));
            }
        }

        if !worlds::is_known(&self.plugin.world) {
            return Err(ManifestError::UnknownWorld(self.plugin.world.clone()));
        }

        // HTTP allowlist non-emptiness is up to the plugin author —
        // an empty list just means "this plugin has no HTTP needs".
        // What we DO check: every kind that's "on" must be in the
        // known catalog.
        if !self.permissions.http.is_empty() && !permissions::is_known(permissions::HTTP) {
            return Err(ManifestError::UnknownPermission(permissions::HTTP.into()));
        }
        if self.permissions.storage_read && !permissions::is_known(permissions::STORAGE_READ) {
            return Err(ManifestError::UnknownPermission(
                permissions::STORAGE_READ.into(),
            ));
        }
        if self.permissions.storage_state && !permissions::is_known(permissions::STORAGE_STATE) {
            return Err(ManifestError::UnknownPermission(
                permissions::STORAGE_STATE.into(),
            ));
        }

        for asset in &self.assets {
            if asset.filename.is_empty() {
                return Err(ManifestError::EmptyAssetFilename);
            }
            // `..` anywhere in the path = sandbox escape. We don't
            // try to normalise — refuse anything suspicious so a
            // forgiving toolchain can't sneak past us.
            if asset.filename.split(['/', '\\']).any(|seg| seg == "..") {
                return Err(ManifestError::AssetEscape(asset.filename.clone()));
            }
            // SHA-256 hex shape: exactly 64 hex digits (case-
            // insensitive — `parse` normalises to lowercase post-
            // validation). Anything else means the author typoed
            // the digest or pasted the wrong line; reject so we
            // don't compare against a malformed expected value
            // and pass through a tampered asset.
            if let Some(hash) = &asset.sha256 {
                if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(ManifestError::InvalidAssetHash(hash.clone()));
                }
            }
        }

        for opt in &self.options {
            if opt.key.is_empty() {
                return Err(ManifestError::EmptyOptionKey);
            }
            let key_ok = opt
                .key
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
            if !key_ok {
                return Err(ManifestError::InvalidOptionKey(opt.key.clone()));
            }
            if !option_types::ALL.contains(&opt.option_type.as_str()) {
                return Err(ManifestError::UnknownOptionType(
                    opt.key.clone(),
                    opt.option_type.clone(),
                ));
            }
            if opt.option_type == option_types::ENUM && opt.choices.is_empty() {
                return Err(ManifestError::EnumOptionWithoutChoices(opt.key.clone()));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(world: &str, http: &[&str]) -> String {
        let http_list = http
            .iter()
            .map(|p| format!("\"{p}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            r#"
schema_version = 1

[plugin]
id = "web-radio"
name = "Web Radio"
version = "1.0.0"
author = "InstaZDLL"
world = "{world}"

[permissions]
http = [{http_list}]
storage_read = true
"#
        )
    }

    #[test]
    fn parse_valid_manifest() {
        let m = Manifest::parse(&fixture(
            worlds::SOURCE_V1,
            &["https://radio-browser.info/*"],
        ))
        .expect("valid manifest");
        assert_eq!(m.plugin.id, "web-radio");
        assert_eq!(m.plugin.world, worlds::SOURCE_V1);
        assert_eq!(m.permissions.http.len(), 1);
        assert!(m.permissions.storage_read);
    }

    #[test]
    fn rejects_unknown_world() {
        let raw = fixture("waveflow:bogus/v1", &[]);
        let err = Manifest::parse(&raw).unwrap_err();
        assert!(matches!(err, ManifestError::UnknownWorld(_)));
    }

    #[test]
    fn parses_valid_options() {
        let raw = format!(
            "{}\n[[options]]\nkey = \"quality\"\ntype = \"enum\"\nlabel = \"Quality\"\ndefault = \"1080\"\nchoices = [\"720\", \"1080\"]\n\n[[options]]\nkey = \"hevc\"\ntype = \"bool\"\nlabel = \"Allow HEVC\"\ndefault = \"false\"\n",
            fixture(worlds::SOURCE_V1, &[])
        );
        let m = Manifest::parse(&raw).expect("valid options");
        assert_eq!(m.options.len(), 2);
        assert_eq!(m.options[0].key, "quality");
        assert_eq!(m.options[0].option_type, "enum");
        assert_eq!(m.options[0].choices, vec!["720", "1080"]);
        assert_eq!(m.options[1].option_type, "bool");
    }

    #[test]
    fn rejects_enum_option_without_choices() {
        let raw = format!(
            "{}\n[[options]]\nkey = \"q\"\ntype = \"enum\"\nlabel = \"Q\"\n",
            fixture(worlds::SOURCE_V1, &[])
        );
        assert!(matches!(
            Manifest::parse(&raw).unwrap_err(),
            ManifestError::EnumOptionWithoutChoices(_)
        ));
    }

    #[test]
    fn rejects_unknown_option_type() {
        let raw = format!(
            "{}\n[[options]]\nkey = \"q\"\ntype = \"slider\"\nlabel = \"Q\"\n",
            fixture(worlds::SOURCE_V1, &[])
        );
        assert!(matches!(
            Manifest::parse(&raw).unwrap_err(),
            ManifestError::UnknownOptionType(_, _)
        ));
    }

    #[test]
    fn rejects_uppercase_id() {
        let raw = r#"
schema_version = 1

[plugin]
id = "WebRadio"
name = "x"
version = "1"
author = "x"
world = "waveflow:source/v1"
"#;
        let err = Manifest::parse(raw).unwrap_err();
        assert!(matches!(err, ManifestError::InvalidIdChar('W')));
    }

    #[test]
    fn rejects_asset_escape() {
        let raw = r#"
schema_version = 1

[plugin]
id = "web-radio"
name = "x"
version = "1"
author = "x"
world = "waveflow:source/v1"

[[assets]]
filename = "../etc/passwd"
"#;
        let err = Manifest::parse(raw).unwrap_err();
        assert!(matches!(err, ManifestError::AssetEscape(_)));
    }

    #[test]
    fn rejects_schema_mismatch() {
        let raw = r#"
schema_version = 9999

[plugin]
id = "web-radio"
name = "x"
version = "1"
author = "x"
world = "waveflow:source/v1"
"#;
        let err = Manifest::parse(raw).unwrap_err();
        assert!(matches!(
            err,
            ManifestError::SchemaVersionMismatch {
                got: 9999,
                expected: 1
            }
        ));
    }
}

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

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use waveflow_plugin_sdk::{permissions, worlds, MANIFEST_SCHEMA_VERSION};

/// A user-facing manifest string that MAY carry per-language variants.
///
/// Two shapes are accepted, and both round-trip through serde
/// untagged — so this type is a drop-in replacement for a plain
/// `String` field without breaking a single existing manifest:
///
/// ```toml
/// # historical form — still valid, treated as one anonymous language
/// description = "Animated album covers from Apple Music."
///
/// # inline localized form
/// [description]
/// en = "Animated album covers from Apple Music."
/// fr = "Pochettes animées depuis Apple Music."
/// ```
///
/// The same two shapes work in the store's `registry.json`
/// (`"description": "…"` or `"description": { "en": "…", … }`).
///
/// **Where resolution happens.** The host hands the whole value to
/// the frontend and the UI resolves it against `i18next.language`,
/// so switching the app language re-renders instantly without a
/// backend round-trip. [`Self::resolve`] is the reference
/// implementation of that fallback chain; its mirror lives in
/// `src/lib/localizedText.ts` and the two MUST agree.
///
/// # The inline form is NOT safe to publish
///
/// A WaveFlow older than the release that introduced this type
/// expects a string and hard-errors on the table: it drops the whole
/// plugin (unreadable manifest), and for `registry.json` — a single
/// document every installed version fetches — it fails to decode the
/// catalogue from all three sources, leaving the store **entirely
/// broken** on that build. `min_app_version` can't rescue that: it is
/// read after the parse that already failed.
///
/// So anything published to users carries the translations in the
/// **sibling `*_i18n` field** instead ([`merge_localized_siblings`]):
///
/// ```toml
/// description = "Animated album covers from Apple Music."
///
/// [plugin.description_i18n]
/// fr = "Pochettes animées depuis Apple Music."
/// ```
///
/// An older host ignores the unknown sibling and keeps rendering the
/// plain string; a current one merges the two before anything else
/// sees the value. No version bump, no broken store. The inline form
/// stays supported for manifests that never ship to older hosts.
///
/// [`merge_localized_siblings`]: Manifest::merge_localized_siblings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LocalizedString {
    /// One language, no code attached. Historically English.
    Plain(String),
    /// `lang code -> text`. `BTreeMap` so serialization order (and
    /// therefore the last-resort fallback below) is deterministic.
    Localized(BTreeMap<String, String>),
}

impl LocalizedString {
    /// Best text for `lang`, following the fallback chain:
    ///
    /// 1. exact match (`pt-BR`),
    /// 2. the base language (`pt-BR` → `pt`),
    /// 3. `en` — the format's documented default,
    /// 4. any entry, lowest key first, so a manifest that ships only
    ///    e.g. `de` still renders something instead of a blank row.
    ///
    /// **Blank entries are skipped at every step** rather than
    /// counting as a hit: `{ fr: "", en: "…" }` resolves to the
    /// English. A translator leaving a slot empty is a common
    /// authoring accident, and letting it win would blank the row —
    /// or leave an option control with no accessible name, since the
    /// UI only falls back to the option key on a `None`.
    ///
    /// `None` when the map is empty or holds nothing renderable.
    /// [`Manifest::validate`] refuses an empty map for manifests, but
    /// a registry entry comes off the network, so callers must handle
    /// it rather than unwrap.
    pub fn resolve(&self, lang: &str) -> Option<&str> {
        /// A translation slot only counts if it holds something to render.
        fn usable(text: Option<&String>) -> Option<&str> {
            text.map(String::as_str).filter(|s| !s.trim().is_empty())
        }
        match self {
            Self::Plain(s) => usable(Some(s)),
            Self::Localized(map) => {
                let base = lang.split('-').next().unwrap_or(lang);
                usable(map.get(lang))
                    .or_else(|| usable(map.get(base)))
                    .or_else(|| usable(map.get("en")))
                    .or_else(|| map.values().find_map(|v| usable(Some(v))))
            }
        }
    }

    /// `true` when the localized form carries no entry at all — a
    /// row the UI could only ever render blank.
    fn is_empty_map(&self) -> bool {
        matches!(self, Self::Localized(map) if map.is_empty())
    }

    /// Fold a sibling `*_i18n` map into `base`, returning the value
    /// every downstream consumer sees.
    ///
    /// The sibling wins per key, so a translation can override an
    /// inline entry. A plain base is promoted to the `en` slot —
    /// which is what the format documents a bare string to mean —
    /// unless the sibling already spells `en` out.
    ///
    /// `None` sibling is the overwhelmingly common case (every
    /// manifest written before this existed) and returns `self`
    /// untouched — not even a reallocation.
    pub fn merged_with(self, sibling: Option<BTreeMap<String, String>>) -> Self {
        let Some(sibling) = sibling else {
            return self;
        };
        let (mut merged, plain) = match self {
            Self::Localized(map) => (map, None),
            Self::Plain(s) => (BTreeMap::new(), Some(s)),
        };
        merged.extend(sibling);
        if let Some(plain) = plain {
            merged.entry("en".to_string()).or_insert(plain);
        }
        Self::Localized(merged)
    }

    /// [`Self::merged_with`] for an optional field: a sibling with no
    /// base of its own still produces a value.
    pub fn merge_optional(
        base: Option<Self>,
        sibling: Option<BTreeMap<String, String>>,
    ) -> Option<Self> {
        match (base, sibling) {
            (base, None) => base,
            (Some(base), sibling) => Some(base.merged_with(sibling)),
            (None, Some(sibling)) => Some(Self::Localized(sibling)),
        }
    }
}

impl From<String> for LocalizedString {
    fn from(s: String) -> Self {
        Self::Plain(s)
    }
}

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
    /// Plain string or `{ lang -> text }` — see [`LocalizedString`].
    pub description: Option<LocalizedString>,
    /// Publish-safe translations for [`Self::description`], folded
    /// into it at parse time. See [`LocalizedString`] for why this
    /// sibling exists rather than only the inline table form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_i18n: Option<BTreeMap<String, String>>,
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
    /// Human-readable label for the settings control. Plain string
    /// or `{ lang -> text }` — see [`LocalizedString`].
    pub label: LocalizedString,
    /// Publish-safe translations for [`Self::label`], folded into it
    /// at parse time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_i18n: Option<BTreeMap<String, String>>,
    /// Default value in string form (`"true"`/`"false"` for bool, one of
    /// `choices` for enum). `None` = no default (control starts empty/off).
    #[serde(default)]
    pub default: Option<String>,
    /// Allowed values — required + only meaningful for `type = "enum"`.
    #[serde(default)]
    pub choices: Vec<String>,
    /// Optional hint rendered under the control. Plain string or
    /// `{ lang -> text }` — see [`LocalizedString`].
    #[serde(default)]
    pub description: Option<LocalizedString>,
    /// Publish-safe translations for [`Self::description`], folded
    /// into it at parse time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_i18n: Option<BTreeMap<String, String>>,
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
    #[error("manifest localized field {0} declares no language at all")]
    EmptyLocalizedMap(String),
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
        // Fold the publish-safe `*_i18n` siblings in BEFORE validating,
        // so validation judges the value the app will actually render
        // and every consumer downstream sees one merged field.
        parsed.merge_localized_siblings();
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

    /// Fold every `*_i18n` sibling into the field it translates. See
    /// [`LocalizedString`] for why publishable manifests carry their
    /// translations this way instead of as an inline table.
    fn merge_localized_siblings(&mut self) {
        self.plugin.description = LocalizedString::merge_optional(
            self.plugin.description.take(),
            self.plugin.description_i18n.take(),
        );
        for opt in &mut self.options {
            if opt.label_i18n.is_some() {
                let base = std::mem::replace(&mut opt.label, LocalizedString::Plain(String::new()));
                opt.label = base.merged_with(opt.label_i18n.take());
            }
            opt.description =
                LocalizedString::merge_optional(opt.description.take(), opt.description_i18n.take());
        }
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

        // A localized field that declares zero languages can only ever
        // render blank, and no fallback can rescue it. Refuse at parse
        // time so the author sees the typo instead of an empty row in
        // Settings. The plain-string form is untouched by this check,
        // so every pre-existing manifest still validates.
        if let Some(desc) = &self.plugin.description {
            if desc.is_empty_map() {
                return Err(ManifestError::EmptyLocalizedMap("plugin.description".into()));
            }
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
            if opt.label.is_empty_map() {
                return Err(ManifestError::EmptyLocalizedMap(format!(
                    "options.{}.label",
                    opt.key
                )));
            }
            if let Some(desc) = &opt.description {
                if desc.is_empty_map() {
                    return Err(ManifestError::EmptyLocalizedMap(format!(
                        "options.{}.description",
                        opt.key
                    )));
                }
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

    // ----- localized strings ------------------------------------------

    /// The historical `description = "…"` / `label = "…"` shape must keep
    /// parsing byte-identically — every published plugin uses it.
    #[test]
    fn plain_strings_stay_valid() {
        // The fixture ends inside [permissions], so plugin.description
        // is injected into the [plugin] table rather than appended.
        let raw = format!(
            "{}\n[[options]]\nkey = \"hevc\"\ntype = \"bool\"\nlabel = \"Prefer HEVC\"\ndescription = \"4K covers\"\n",
            fixture(worlds::SOURCE_V1, &[]).replace(
                "world = \"waveflow:source/v1\"\n",
                "world = \"waveflow:source/v1\"\ndescription = \"Play internet radio\"\n",
            )
        );
        let m = Manifest::parse(&raw).expect("plain strings still parse");
        assert_eq!(
            m.plugin.description.as_ref().unwrap().resolve("fr"),
            Some("Play internet radio"),
            "a plain string resolves for every language"
        );
        assert_eq!(m.options[0].label.resolve("ja"), Some("Prefer HEVC"));
        assert_eq!(
            m.options[0].description.as_ref().unwrap().resolve("de"),
            Some("4K covers")
        );
    }

    #[test]
    fn parses_localized_description_and_option_label() {
        let raw = r#"
schema_version = 1

[plugin]
id = "apple-artwork"
name = "Apple Motion Artwork"
version = "0.3.0"
author = "InstaZDLL"
world = "waveflow:metadata/v1"

[plugin.description]
en = "Animated album covers."
fr = "Pochettes animées."

[[options]]
key = "prefer_hevc"
type = "bool"
default = "false"

[options.label]
en = "Prefer 4K HEVC covers"
fr = "Préférer les pochettes 4K HEVC"

[options.description]
en = "Bigger files."
"#;
        let m = Manifest::parse(raw).expect("localized manifest");
        let desc = m.plugin.description.as_ref().unwrap();
        assert_eq!(desc.resolve("fr"), Some("Pochettes animées."));
        assert_eq!(desc.resolve("en"), Some("Animated album covers."));
        assert_eq!(m.options[0].label.resolve("fr"), Some("Préférer les pochettes 4K HEVC"));
        // Missing locale on an option description falls back to `en`.
        assert_eq!(
            m.options[0].description.as_ref().unwrap().resolve("ja"),
            Some("Bigger files.")
        );
    }

    #[test]
    fn resolve_follows_the_documented_fallback_chain() {
        let mut map = BTreeMap::new();
        map.insert("en".to_string(), "english".to_string());
        map.insert("pt".to_string(), "português".to_string());
        map.insert("pt-BR".to_string(), "português do Brasil".to_string());
        let s = LocalizedString::Localized(map);

        assert_eq!(s.resolve("pt-BR"), Some("português do Brasil"), "exact wins");
        assert_eq!(s.resolve("pt"), Some("português"), "exact base code wins");
        assert_eq!(
            s.resolve("fr-CA"),
            Some("english"),
            "unknown regional code falls through base to en"
        );
        assert_eq!(s.resolve("ja"), Some("english"), "unknown code falls back to en");

        // No `en` at all: any entry beats rendering nothing. BTreeMap
        // ordering makes the pick deterministic.
        let mut only_de = BTreeMap::new();
        only_de.insert("de".to_string(), "deutsch".to_string());
        assert_eq!(
            LocalizedString::Localized(only_de).resolve("fr"),
            Some("deutsch")
        );
    }

    /// An empty slot is an authoring accident, not a translation:
    /// letting it win blanks the row (and strips an option control of
    /// its accessible name, since the UI only falls back on `None`).
    #[test]
    fn resolve_skips_blank_entries() {
        let s = LocalizedString::Localized(BTreeMap::from([
            ("fr".to_string(), String::new()),
            ("de".to_string(), "   ".to_string()),
            ("en".to_string(), "english".to_string()),
        ]));
        assert_eq!(s.resolve("fr"), Some("english"), "empty falls through to en");
        assert_eq!(s.resolve("de"), Some("english"), "whitespace-only too");

        // Nothing renderable anywhere: report None so the caller can
        // substitute its own label rather than render an empty string.
        let blank = LocalizedString::Localized(BTreeMap::from([
            ("en".to_string(), String::new()),
            ("fr".to_string(), "  ".to_string()),
        ]));
        assert_eq!(blank.resolve("fr"), None);
        assert_eq!(LocalizedString::Plain(String::new()).resolve("en"), None);
    }

    /// The publish-safe shape: a plain string an older host still
    /// renders, plus a sibling map it doesn't know about. Both must
    /// land in one merged value here, with the plain string taking
    /// the `en` slot.
    #[test]
    fn folds_i18n_siblings_into_their_field() {
        let raw = r#"
schema_version = 1

[plugin]
id = "apple-artwork"
name = "Apple Motion Artwork"
version = "0.4.0"
author = "InstaZDLL"
world = "waveflow:metadata/v1"
description = "Animated album covers."

[plugin.description_i18n]
fr = "Pochettes animées."

[[options]]
key = "prefer_hevc"
type = "bool"
label = "Prefer 4K HEVC covers"
description = "Bigger files."

[options.label_i18n]
fr = "Préférer les pochettes 4K HEVC"

[options.description_i18n]
fr = "Fichiers plus lourds."
"#;
        let m = Manifest::parse(raw).expect("sibling form parses");
        let desc = m.plugin.description.as_ref().unwrap();
        assert_eq!(desc.resolve("fr"), Some("Pochettes animées."));
        assert_eq!(
            desc.resolve("en"),
            Some("Animated album covers."),
            "the plain string becomes the en entry"
        );
        assert_eq!(desc.resolve("ja"), Some("Animated album covers."));
        assert_eq!(
            m.options[0].label.resolve("fr"),
            Some("Préférer les pochettes 4K HEVC")
        );
        assert_eq!(m.options[0].label.resolve("de"), Some("Prefer 4K HEVC covers"));
        assert_eq!(
            m.options[0].description.as_ref().unwrap().resolve("fr"),
            Some("Fichiers plus lourds.")
        );
        // The siblings are consumed by the merge, not echoed onward.
        assert!(m.plugin.description_i18n.is_none());
        assert!(m.options[0].label_i18n.is_none());
    }

    /// A sibling entry overrides the same language spelled inline —
    /// the sibling is the newer, translator-maintained source.
    #[test]
    fn sibling_wins_over_an_inline_entry_for_the_same_language() {
        let s = LocalizedString::Localized(BTreeMap::from([
            ("en".to_string(), "inline english".to_string()),
            ("fr".to_string(), "français inline".to_string()),
        ]));
        let merged = s.merged_with(Some(BTreeMap::from([(
            "fr".to_string(),
            "français traduit".to_string(),
        )])));
        assert_eq!(merged.resolve("fr"), Some("français traduit"));
        assert_eq!(merged.resolve("en"), Some("inline english"));
    }

    #[test]
    fn rejects_localized_field_without_any_language() {
        let raw = format!(
            "{}\n[[options]]\nkey = \"q\"\ntype = \"bool\"\n\n[options.label]\n",
            fixture(worlds::SOURCE_V1, &[])
        );
        let err = Manifest::parse(&raw).unwrap_err();
        assert!(
            matches!(&err, ManifestError::EmptyLocalizedMap(f) if f == "options.q.label"),
            "got {err:?}"
        );
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

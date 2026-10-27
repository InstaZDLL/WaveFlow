//! A plugin's options: reading them, masked where they are secret, and
//! setting one.

use serde::Serialize;
use tauri::State;
use waveflow_core::plugin::manifest::{LocalizedString, Manifest};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

use super::*;

// ----- plugin options (Phase 2 — waveflow:host/config) ---------------------

/// One user-configurable option: the manifest declaration merged with the
/// user's current value. Mirrors the frontend `PluginOption`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginOption {
    pub key: String,
    #[serde(rename = "type")]
    pub option_type: String,
    /// Plain string or `{ lang -> text }`, resolved frontend-side.
    pub label: LocalizedString,
    pub default: Option<String>,
    pub choices: Vec<String>,
    /// Plain string or `{ lang -> text }`, resolved frontend-side.
    pub description: Option<LocalizedString>,
    /// Current stored value; `None` = unset (the plugin uses `default`).
    /// Always `None` for a sensitive option: see [`Self::is_set`].
    pub value: Option<String>,
    /// A credential: masked in the panel, its value never sent here.
    pub sensitive: bool,
    /// Whether a value is stored. The one thing the panel learns about a
    /// sensitive option, so it can say "saved" without holding the token.
    pub is_set: bool,
}

/// Validate a proposed option `value` against its manifest declaration.
fn validate_option_value(
    decl: &waveflow_core::plugin::manifest::OptionDecl,
    value: &str,
) -> AppResult<()> {
    use waveflow_core::plugin::manifest::option_types;
    match decl.option_type.as_str() {
        option_types::BOOL if value != "true" && value != "false" => Err(AppError::Other(format!(
            "option {} expects \"true\" or \"false\"",
            decl.key
        ))),
        option_types::ENUM if !decl.choices.iter().any(|c| c == value) => Err(AppError::Other(
            format!("option {} value not in declared choices", decl.key),
        )),
        _ => Ok(()),
    }
}

/// List a plugin's declared options merged with the user's current values.
/// Empty vec when the plugin declares none (or the id doesn't resolve).
#[tauri::command]
pub async fn get_plugin_options(
    state: State<'_, AppState>,
    plugin_id: String,
) -> AppResult<Vec<PluginOption>> {
    validate_plugin_id_chars(&plugin_id)?;
    let paths = state.paths.plugin_paths();
    let manifest_path = paths
        .manifest_path(&plugin_id)
        .map_err(|e| AppError::Other(format!("invalid plugin id: {e}")))?;
    let state_dir = paths
        .state_dir(&plugin_id)
        .map_err(|e| AppError::Other(format!("invalid plugin id: {e}")))?;
    let id_for_blocking = plugin_id.clone();
    tokio::task::spawn_blocking(move || -> AppResult<Vec<PluginOption>> {
        let manifest = Manifest::load_from_path(&manifest_path)
            .map_err(|e| AppError::Other(format!("load manifest: {e}")))?;
        if manifest.plugin.id != id_for_blocking {
            return Err(AppError::Other(
                "manifest id does not match plugin dir".into(),
            ));
        }
        let values = waveflow_core::plugin::plugin_config::read(&state_dir);
        Ok(manifest
            .options
            .into_iter()
            .map(|o| to_plugin_option(o, &values))
            .collect())
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))?
}

/// One manifest option, as the panel is allowed to see it.
///
/// A sensitive `text` option leaves its stored value behind: the panel
/// learns only whether one is set (`is_set`). That is the point of the
/// flag — a credential shown nowhere and never sent to the webview. Only a
/// text option is a credential: a `bool` or `enum` flagged sensitive by
/// mistake keeps its value, or the panel would show the default instead of
/// the setting.
pub(super) fn to_plugin_option(
    o: waveflow_core::plugin::manifest::OptionDecl,
    values: &std::collections::HashMap<String, String>,
) -> PluginOption {
    let sensitive =
        o.sensitive && o.option_type == waveflow_core::plugin::manifest::option_types::TEXT;
    PluginOption {
        is_set: values.get(&o.key).is_some_and(|v| !v.is_empty()),
        value: if sensitive {
            None
        } else {
            values.get(&o.key).cloned()
        },
        sensitive,
        key: o.key,
        option_type: o.option_type,
        label: o.label,
        default: o.default,
        choices: o.choices,
        description: o.description,
    }
}

/// Set (or reset, when `value` is `None`) one plugin option. Validates the
/// value against the manifest declaration, then rewrites the plugin's config
/// file. The new value takes effect the next time the plugin is instantiated
/// (the runtime reloads config per call).
#[tauri::command]
pub async fn set_plugin_option(
    state: State<'_, AppState>,
    plugin_id: String,
    key: String,
    value: Option<String>,
) -> AppResult<()> {
    validate_plugin_id_chars(&plugin_id)?;
    let paths = state.paths.plugin_paths();
    let manifest_path = paths
        .manifest_path(&plugin_id)
        .map_err(|e| AppError::Other(format!("invalid plugin id: {e}")))?;
    let state_dir = paths
        .state_dir(&plugin_id)
        .map_err(|e| AppError::Other(format!("invalid plugin id: {e}")))?;
    // Serialise against enable/uninstall/install (+ concurrent option writes)
    // for this id via the shared per-plugin lock.
    let _guard = lock_plugin(&state, &plugin_id).await;
    let id_for_blocking = plugin_id.clone();
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        let manifest = Manifest::load_from_path(&manifest_path)
            .map_err(|e| AppError::Other(format!("load manifest: {e}")))?;
        if manifest.plugin.id != id_for_blocking {
            return Err(AppError::Other(
                "manifest id does not match plugin dir".into(),
            ));
        }
        let decl = manifest
            .options
            .iter()
            .find(|o| o.key == key)
            .ok_or_else(|| AppError::Other(format!("unknown option: {key}")))?;
        if let Some(v) = &value {
            validate_option_value(decl, v)?;
        }
        let mut values = waveflow_core::plugin::plugin_config::read(&state_dir);
        match value {
            Some(v) => {
                values.insert(key, v);
            }
            None => {
                values.remove(&key); // reset to the manifest default
            }
        }
        waveflow_core::plugin::plugin_config::write(&state_dir, &values).map_err(AppError::Io)?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))?
}

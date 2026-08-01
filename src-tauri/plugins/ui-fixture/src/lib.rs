//! WaveFlow UI-world test fixture — guest side.
//!
//! A deliberately tiny `waveflow:ui/v1` plugin whose only job is to
//! let the host's integration test exercise the UI world end-to-end:
//!
//! - `manifest()` returns a fixed sidebar mount point.
//! - `render(path)` calls the redacted `library.list-artists` host
//!   import and folds the result into a JSON view descriptor — on a
//!   permission-denied manifest the host returns `Err`, which we
//!   surface as `status = "error"`, so the SAME wasm proves BOTH the
//!   granted and denied paths (the test stages two manifests).
//! - `on-event(event, payload)` echoes the action back into a fresh
//!   descriptor, exercising the render → action → re-render loop.
//!
//! This crate is NOT shipped — see `Cargo.toml`.

#[allow(warnings)]
mod bindings;

use bindings::exports::waveflow::ui::extension::{Guest, MountPoint};
use bindings::waveflow::host::library;

use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    schema_version: u32,
    title: String,
    subtitle: String,
    /// `"fresh"` when the artist read succeeded, `"error"` when the
    /// host denied `library.read_artists`.
    status: String,
    sections: Vec<Section>,
    empty_hint: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Section {
    title: String,
    items: Vec<Item>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    id: String,
    title: String,
    subtitle: String,
}

struct Fixture;

impl Guest for Fixture {
    fn manifest() -> MountPoint {
        MountPoint {
            sidebar_label: "UI Fixture".to_string(),
            sidebar_icon: Some("radar".to_string()),
            initial_path: "/".to_string(),
        }
    }

    fn render(path: String) -> Result<String, String> {
        // Request far more than the host's cap so the integration test
        // can exercise host-side clamping (the host returns at most
        // MAX_LIBRARY_ARTISTS regardless of what we ask for).
        let (status, items, empty_hint) = match library::list_artists(u32::MAX) {
            Ok(artists) => (
                "fresh",
                artists
                    .into_iter()
                    .map(|a| Item {
                        id: a.id.to_string(),
                        title: a.name,
                        subtitle: format!("{} tracks", a.track_count),
                    })
                    .collect::<Vec<_>>(),
                String::new(),
            ),
            Err(e) => ("error", Vec::new(), e),
        };
        let descriptor = Descriptor {
            schema_version: 1,
            title: "UI Fixture".to_string(),
            subtitle: format!("path={path}"),
            status: status.to_string(),
            sections: vec![Section {
                title: "Artists".to_string(),
                items,
            }],
            empty_hint,
        };
        serde_json::to_string(&descriptor).map_err(|e| e.to_string())
    }

    fn on_event(event: String, payload: String) -> Result<String, String> {
        let descriptor = Descriptor {
            schema_version: 1,
            title: format!("event:{event}"),
            subtitle: format!("payload={payload}"),
            status: "fresh".to_string(),
            sections: Vec::new(),
            empty_hint: String::new(),
        };
        serde_json::to_string(&descriptor).map_err(|e| e.to_string())
    }
}

bindings::export!(Fixture with_types_in bindings);

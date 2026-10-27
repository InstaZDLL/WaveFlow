use super::*;

/// Decode a catalogue the way [`fetch_registry`] does, then fold
/// the sibling in the way [`list_plugin_marketplace`] does — the
/// one place where a published registry meets the store row.
fn resolve_description(json: &str, lang: &str) -> Option<String> {
    let reg: Registry = serde_json::from_str(json).expect("registry decodes");
    let e = reg.plugins.into_iter().next().expect("one entry");
    let merged = e.description.merged_with(e.description_i18n);
    merged.resolve(lang).map(str::to_string)
}

fn registry_json(description_field: &str) -> String {
    format!(
        r#"{{
              "schema_version": 1,
              "plugins": [{{
                "id": "apple-artwork",
                "name": "Apple Motion Artwork",
                {description_field},
                "author": "InstaZDLL",
                "repo": "InstaZDLL/waveflow-plugin-apple-artwork",
                "world": "waveflow:metadata/v1",
                "version": "0.3.0",
                "blake3": "3a369e9f03bc7679ca86973f29ce78a8243db1f54ce41cbd936af734318e7e51",
                "permissions": {{ "http": [], "storage_read": false, "storage_state": true }}
              }}]
            }}"#
    )
}

/// The shape every published registry uses: a plain string plus a
/// sibling map. This is the compatibility contract the whole
/// feature rests on, so it gets its own test at the seam.
#[test]
fn registry_entry_merges_its_i18n_sibling() {
    let json = registry_json(
        r#""description": "Animated album covers.",
               "description_i18n": { "fr": "Pochettes animées." }"#,
    );
    assert_eq!(
        resolve_description(&json, "fr").as_deref(),
        Some("Pochettes animées.")
    );
    // The plain string takes the `en` slot, so untranslated
    // languages still read the author's original text.
    assert_eq!(
        resolve_description(&json, "en").as_deref(),
        Some("Animated album covers.")
    );
    assert_eq!(
        resolve_description(&json, "ja").as_deref(),
        Some("Animated album covers.")
    );
}

/// Every entry published to date, and the only form older clients
/// can read — it must survive the merge untouched.
#[test]
fn registry_entry_without_a_sibling_is_unchanged() {
    let json = registry_json(r#""description": "Animated album covers.""#);
    for lang in ["en", "fr", "zh-CN"] {
        assert_eq!(
            resolve_description(&json, lang).as_deref(),
            Some("Animated album covers.")
        );
    }
}

/// A hostile or malformed catalogue can carry an unrenderable
/// description; the store row must degrade to "no description"
/// rather than an empty paragraph holding its margin.
#[test]
fn registry_entry_with_nothing_renderable_resolves_to_none() {
    let json = registry_json(r#""description": "", "description_i18n": { "fr": "  " }"#);
    assert_eq!(resolve_description(&json, "fr"), None);
}

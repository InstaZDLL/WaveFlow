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
fn a_plugin_without_provides_answers_everything() {
    let m = Manifest::parse(&fixture(worlds::METADATA_V3, &[])).expect("valid manifest");
    assert!(m.plugin.provides.is_empty());
    assert!(m.plugin.provides(functions::LYRICS));
    assert!(m.plugin.provides(functions::ALBUM_INFO));
}

#[test]
fn provides_narrows_to_the_functions_named() {
    let raw = fixture(worlds::METADATA_V3, &[]).replace(
        "world = \"waveflow:metadata/v3\"\n",
        "world = \"waveflow:metadata/v3\"\nprovides = [\"album-info\", \"a-newer-function\"]\n",
    );
    let m = Manifest::parse(&raw).expect("an unknown function name is not an error");
    assert!(m.plugin.provides(functions::ALBUM_INFO));
    assert!(!m.plugin.provides(functions::LYRICS));
    assert!(!m.plugin.provides(functions::ARTIST_INFO));
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

/// A credential is flagged on a plain `text` option, and an option
/// that does not say so is not one.
#[test]
fn a_sensitive_option_is_a_flag_on_text() {
    let raw = format!(
        "{}
[[options]]
key = \"token\"
type = \"text\"
label = \"Token\"
sensitive = true

[[options]]
key = \"lang\"
type = \"text\"
label = \"Language\"
",
        fixture(worlds::SOURCE_V1, &[])
    );
    let m = Manifest::parse(&raw).expect("valid options");
    assert!(m.options[0].sensitive);
    assert!(!m.options[1].sensitive);
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
    assert_eq!(
        m.options[0].label.resolve("fr"),
        Some("Préférer les pochettes 4K HEVC")
    );
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

    assert_eq!(
        s.resolve("pt-BR"),
        Some("português do Brasil"),
        "exact wins"
    );
    assert_eq!(s.resolve("pt"), Some("português"), "exact base code wins");
    assert_eq!(
        s.resolve("fr-CA"),
        Some("english"),
        "unknown regional code falls through base to en"
    );
    assert_eq!(
        s.resolve("ja"),
        Some("english"),
        "unknown code falls back to en"
    );

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
    assert_eq!(
        s.resolve("fr"),
        Some("english"),
        "empty falls through to en"
    );
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
    assert_eq!(
        m.options[0].label.resolve("de"),
        Some("Prefer 4K HEVC covers")
    );
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

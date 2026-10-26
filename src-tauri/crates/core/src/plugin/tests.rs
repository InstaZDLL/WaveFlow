use super::*;

#[test]
fn plugin_paths_compose_under_root() {
    let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"));
    assert_eq!(paths.plugins_root, Path::new("/tmp/waveflow/plugins"));
    assert_eq!(paths.data_root, Path::new("/tmp/waveflow/plugin-data"));
    assert_eq!(
        paths.manifest_path("web-radio").unwrap(),
        Path::new("/tmp/waveflow/plugins/web-radio/manifest.toml")
    );
    assert_eq!(
        paths.assets_dir("web-radio").unwrap(),
        Path::new("/tmp/waveflow/plugins/web-radio/assets")
    );
    assert_eq!(
        paths.wasm_path("web-radio").unwrap(),
        Path::new("/tmp/waveflow/plugins/web-radio/plugin.wasm")
    );
    assert_eq!(
        paths.state_dir("web-radio").unwrap(),
        Path::new("/tmp/waveflow/plugin-data/web-radio")
    );
}

#[test]
fn state_dir_shares_sanitisation_with_plugin_dir() {
    let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"));
    assert!(paths.state_dir("/etc/passwd").is_err());
    assert!(paths.state_dir("..").is_err());
    assert!(paths.state_dir("web-radio/subdir").is_err());
    assert!(paths.state_dir("").is_err());
}

#[test]
fn bundled_plugin_routes_to_bundled_root() {
    // When `bundled_root` is set, a bundled id (`web-radio` per
    // [`BUNDLED_PLUGINS`]) resolves under it for install + manifest
    // + wasm + assets — but NOT for state_dir, which always stays
    // in the writable app-data tree so user state survives an
    // install dir refresh.
    let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"))
        .with_bundled_root(Some(PathBuf::from("/opt/waveflow/resources/plugins")));
    assert_eq!(
        paths.plugin_dir("web-radio").unwrap(),
        Path::new("/opt/waveflow/resources/plugins/web-radio")
    );
    assert_eq!(
        paths.manifest_path("web-radio").unwrap(),
        Path::new("/opt/waveflow/resources/plugins/web-radio/manifest.toml")
    );
    assert_eq!(
        paths.wasm_path("web-radio").unwrap(),
        Path::new("/opt/waveflow/resources/plugins/web-radio/plugin.wasm")
    );
    assert_eq!(
        paths.assets_dir("web-radio").unwrap(),
        Path::new("/opt/waveflow/resources/plugins/web-radio/assets")
    );
    // State lives in the writable tree even for bundled plugins.
    assert_eq!(
        paths.state_dir("web-radio").unwrap(),
        Path::new("/tmp/waveflow/plugin-data/web-radio")
    );
}

#[test]
fn sideloaded_plugin_stays_in_plugins_root_even_with_bundled_set() {
    // Non-bundled ids always resolve under `plugins_root` (the
    // sideloaded tree), regardless of whether a `bundled_root`
    // was injected. This is what keeps user-installed plugins
    // working alongside first-party bundled ones.
    let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"))
        .with_bundled_root(Some(PathBuf::from("/opt/waveflow/resources/plugins")));
    assert_eq!(
        paths.plugin_dir("my-custom").unwrap(),
        Path::new("/tmp/waveflow/plugins/my-custom")
    );
}

#[test]
fn bundled_plugin_falls_back_to_plugins_root_without_bundled_set() {
    // Tests + core-only contexts that never resolve a resource
    // dir must still be able to call `plugin_dir("web-radio")`
    // and get a deterministic path back — fall back to
    // `plugins_root` so existing fixtures keep working.
    let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"));
    assert!(paths.bundled_root.is_none());
    assert_eq!(
        paths.plugin_dir("web-radio").unwrap(),
        Path::new("/tmp/waveflow/plugins/web-radio")
    );
}

#[test]
fn is_bundled_plugin_only_matches_known_ids() {
    assert!(is_bundled_plugin("web-radio"));
    assert!(!is_bundled_plugin("web-Radio")); // case-sensitive
    assert!(!is_bundled_plugin("not-bundled"));
    assert!(!is_bundled_plugin(""));
}

#[test]
fn plugin_dir_rejects_absolute_id() {
    let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"));
    // Unix-style absolute id — would otherwise escape into /etc.
    assert!(paths.plugin_dir("/etc/passwd").is_err());
}

#[test]
fn plugin_dir_rejects_parent_segment() {
    let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"));
    // `..` would walk up out of the plugins directory.
    assert!(paths.plugin_dir("../escape").is_err());
    assert!(paths.plugin_dir("..").is_err());
}

#[test]
fn plugin_dir_rejects_embedded_separator() {
    let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"));
    // Even without `..`, an embedded `/` walks into a sub-tree
    // the host wasn't expecting (e.g. mounting an asset path as
    // a plugin id).
    assert!(paths.plugin_dir("web-radio/subdir").is_err());
}

#[test]
fn plugin_dir_rejects_empty() {
    let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"));
    assert!(paths.plugin_dir("").is_err());
}

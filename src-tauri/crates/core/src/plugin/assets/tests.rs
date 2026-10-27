use super::*;
use crate::plugin::manifest::{AssetDecl, Manifest, PluginMetadata};

fn make_manifest(assets: Vec<AssetDecl>) -> Manifest {
    // Build via the public types rather than parse — we only
    // care about the resolver's behaviour here, not the
    // manifest validator.
    Manifest {
        schema_version: 1,
        plugin: PluginMetadata {
            id: "test".into(),
            name: "Test".into(),
            version: "1.0.0".into(),
            author: "x".into(),
            world: waveflow_plugin_sdk::worlds::SOURCE_V1.into(),
            description: None,
            description_i18n: None,
            homepage: None,
            license: None,
            provides: Vec::new(),
        },
        permissions: Default::default(),
        assets,
        options: Vec::new(),
    }
}

#[test]
fn rejects_undeclared_asset() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("assets")).unwrap();
    std::fs::write(tmp.path().join("assets").join("hello.txt"), b"hi").unwrap();
    let manifest = make_manifest(vec![]); // no assets declared
    let r = AssetResolver::new(tmp.path(), &manifest);
    let err = r.read("hello.txt").unwrap_err();
    assert!(matches!(err, AssetError::NotDeclared(_)));
}

#[test]
fn reads_declared_asset() {
    let tmp = tempfile::tempdir().unwrap();
    let assets = tmp.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("stations.db"), b"sqlite-bytes").unwrap();
    let manifest = make_manifest(vec![AssetDecl {
        filename: "stations.db".into(),
        description: None,
        sha256: None,
    }]);
    let r = AssetResolver::new(tmp.path(), &manifest);
    let bytes = r.read("stations.db").expect("declared asset");
    assert_eq!(bytes, b"sqlite-bytes");
}

#[test]
fn detects_sha256_tampering() {
    let tmp = tempfile::tempdir().unwrap();
    let assets = tmp.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("file"), b"original").unwrap();
    // Hash of "tampered", not "original".
    let bogus_hash = "0c8f0c79c8f7b6e0f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9f9".to_string();
    let manifest = make_manifest(vec![AssetDecl {
        filename: "file".into(),
        description: None,
        sha256: Some(bogus_hash),
    }]);
    let r = AssetResolver::new(tmp.path(), &manifest);
    let err = r.read("file").unwrap_err();
    assert!(matches!(err, AssetError::HashMismatch { .. }));
}

#[test]
fn rejects_path_traversal_at_runtime() {
    // Belt-and-suspenders: the manifest validator already rejects
    // ".." segments in asset filenames, but `AssetResolver::read`
    // is callable directly from the host import boundary with
    // whatever string the wasm plugin chose to pass at runtime.
    // A plugin asking for "../secret.txt" must hit the
    // not-declared branch (the manifest never listed it) and not
    // the canonicalisation fallback below.
    let tmp = tempfile::tempdir().unwrap();
    let assets = tmp.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("ok.txt"), b"declared").unwrap();
    // A "secret" file living NEXT TO the assets directory — what
    // a "../secret.txt" lookup would resolve to if the
    // sandboxing didn't bite.
    std::fs::write(tmp.path().join("secret.txt"), b"sensitive").unwrap();
    let manifest = make_manifest(vec![AssetDecl {
        filename: "ok.txt".into(),
        description: None,
        sha256: None,
    }]);
    let r = AssetResolver::new(tmp.path(), &manifest);
    let err = r.read("../secret.txt").unwrap_err();
    assert!(matches!(err, AssetError::NotDeclared(_)));
}

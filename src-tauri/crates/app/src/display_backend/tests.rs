use super::*;

#[test]
fn a_missing_or_broken_file_reads_as_native_wayland() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE);
    assert!(!read(&path).xwayland);
    std::fs::write(&path, b"{not json").unwrap();
    assert!(!read(&path).xwayland);
    std::fs::write(&path, br#"{"xwayland":true}"#).unwrap();
    assert!(read(&path).xwayland);
}

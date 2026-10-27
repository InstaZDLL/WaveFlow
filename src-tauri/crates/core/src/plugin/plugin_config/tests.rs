use super::*;

#[test]
fn missing_file_reads_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(read(dir.path()).is_empty());
}

#[test]
fn write_then_read_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let mut values = HashMap::new();
    values.insert("quality".to_string(), "high".to_string());
    values.insert("enabled".to_string(), "true".to_string());
    write(dir.path(), &values).unwrap();
    assert_eq!(read(dir.path()), values);
    // No temp file left behind after the atomic publish.
    assert!(!dir.path().join(format!("{CONFIG_FILE}.tmp")).exists());
}

#[test]
fn malformed_json_reads_empty() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(CONFIG_FILE), b"{ not json").unwrap();
    assert!(read(dir.path()).is_empty());
}

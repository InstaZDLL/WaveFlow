use super::*;

fn write(path: &Path, len: usize) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![0u8; len]).unwrap();
}

#[test]
fn databases_snapshots_and_backups_are_told_apart() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("app.db"), 10);
    write(&root.join("app.db-wal"), 5);
    write(&root.join("app.db.pre-20260101000000.bak"), 7);
    write(&root.join("profiles/1/data.db"), 20);
    write(&root.join("profiles/1/data.db-shm"), 3);
    write(&root.join("profiles/1/data.db.pre-20260101000000.bak"), 11);
    write(&root.join("profiles/1/artwork/cover.jpg"), 100);
    write(&root.join("backups/Main-2026.waveflow"), 50);
    write(&root.join("backups/notes.txt"), 9);

    let db_dirs = vec![root.to_path_buf(), root.join("profiles/1")];
    let usage = measure(vec![
        Item::files_in("databases", db_dirs.clone(), is_database),
        Item::files_in("snapshots", db_dirs, is_database_snapshot),
        Item::files_in("backups", vec![root.join("backups")], is_backup_archive),
        Item::tree("cache", vec![root.join("missing")], true),
        Item::tree("artwork", vec![root.join("profiles")], false),
    ]);
    let get = |id: &str| usage.entries.iter().find(|e| e.id == id).unwrap();
    assert_eq!((get("databases").bytes, get("databases").files), (38, 4));
    assert_eq!((get("snapshots").bytes, get("snapshots").files), (18, 2));
    // A file of the user's beside the archives is not counted.
    assert_eq!((get("backups").bytes, get("backups").files), (50, 1));
    // A directory that is not there is empty, not an error.
    assert_eq!((get("cache").bytes, get("cache").files), (0, 0));
    assert!(!get("cache").incomplete);
    // A whole tree, by contrast, counts everything under it.
    assert_eq!(get("artwork").files, 4);
    assert_eq!(usage.total_bytes, 38 + 18 + 50 + (20 + 3 + 11 + 100));
}

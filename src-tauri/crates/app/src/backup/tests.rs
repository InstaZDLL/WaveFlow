use super::*;

#[test]
fn a_finished_archive_of_the_profile_is_counted() {
    assert_eq!(
        classify_backup_file("Main-2026-10-19T10-00-00.waveflow", "Main-"),
        BackupFile::Archive
    );
}

/// An unfinished archive must never count toward retention: kept as
/// the newest, it pushed the oldest good backup out.
#[test]
fn an_unfinished_archive_is_not_a_backup() {
    assert_eq!(
        classify_backup_file("Main-2026-10-19T10-00-00.4242-1.waveflow.partial", "Main-"),
        BackupFile::Partial
    );
}

#[test]
fn other_files_are_left_alone() {
    assert_eq!(
        classify_backup_file("Other-2026-10-19T10-00-00.waveflow", "Main-"),
        BackupFile::Other
    );
    assert_eq!(
        classify_backup_file("Main-notes.txt.partial", "Main-"),
        BackupFile::Other
    );
    assert_eq!(
        classify_backup_file("Main-notes.txt", "Main-"),
        BackupFile::Other
    );
}

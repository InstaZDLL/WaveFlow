use super::ArtworkSource::{self, Deezer, Embedded, Folder, Manual};

/// The migration that created `artwork`, read from the real file
/// rather than copied: a copy is what let "theaudiodb" through (#750),
/// since every fixture in this crate re-typed the constraint by hand.
const INITIAL_PROFILE_MIGRATION: &str =
    include_str!("../../../../../../migrations/profile/20260411120000_initial.sql");

fn every_variant() -> Vec<ArtworkSource> {
    // A new variant makes this match non-exhaustive, so it cannot be
    // added without passing through here.
    let _ = |s: ArtworkSource| match s {
        Embedded | Folder | Deezer | Manual => (),
    };
    vec![Embedded, Folder, Deezer, Manual]
}

/// The values the `artwork.source` CHECK accepts, parsed out of the
/// `CREATE TABLE artwork` statement.
fn allowed_by_the_constraint() -> Vec<String> {
    let table = INITIAL_PROFILE_MIGRATION
        .split("CREATE TABLE artwork")
        .nth(1)
        .expect("the initial migration creates the artwork table");
    let list = table
        .split("source IN (")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .expect("artwork.source carries a CHECK (source IN (...))");
    list.split(',')
        .map(|v| v.trim().trim_matches('\'').to_string())
        .collect()
}

#[test]
fn every_source_is_one_the_database_accepts() {
    let allowed = allowed_by_the_constraint();
    for source in every_variant() {
        assert!(
            allowed.iter().any(|a| a == source.as_str()),
            "{:?} writes {:?}, which artwork.source's CHECK refuses (allowed: {allowed:?})",
            source,
            source.as_str(),
        );
    }
}

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

#[test]
fn the_front_cover_is_the_one_extracted_wherever_it_sits() {
    use lofty::picture::{MimeType, Picture, PictureType};
    use lofty::tag::{Tag, TagType};

    let picture = |kind, byte: u8| {
        Picture::unchecked(vec![byte; 32])
            .pic_type(kind)
            .mime_type(MimeType::Png)
            .build()
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let extracted = |tag: &Tag| super::extract_cover(tag, dir.path()).expect("a cover").hash;

    let mut replaced = Tag::new(TagType::VorbisComments);
    replaced.push_picture(picture(PictureType::CoverBack, 0x22));
    replaced.push_picture(picture(PictureType::CoverFront, 0x11));
    assert_eq!(
        extracted(&replaced),
        blake3::hash(&[0x11; 32]).to_hex().to_string()
    );

    // An empty front cover behind a real image does not hide it.
    let mut hollow = Tag::new(TagType::VorbisComments);
    hollow.push_picture(picture(PictureType::Other, 0x33));
    hollow.push_picture(
        Picture::unchecked(Vec::new())
            .pic_type(PictureType::CoverFront)
            .mime_type(MimeType::Png)
            .build(),
    );
    assert_eq!(
        extracted(&hollow),
        blake3::hash(&[0x33; 32]).to_hex().to_string()
    );

    // No front cover among them: the first, as ever.
    let mut unmarked = Tag::new(TagType::VorbisComments);
    unmarked.push_picture(picture(PictureType::Other, 0x33));
    unmarked.push_picture(picture(PictureType::CoverBack, 0x22));
    assert_eq!(
        extracted(&unmarked),
        blake3::hash(&[0x33; 32]).to_hex().to_string()
    );
}

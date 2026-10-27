//! The patch written into a DSF file, whose tag sits after the audio.

use super::*;

/// Write a patch into a DSF file (#592).
///
/// The easy container, once someone writes the twenty lines for it: the
/// tag lives *after* the audio at an offset the header declares, so it
/// can grow or shrink without a single audio byte moving. That is why
/// this path does not need the in-place/rewrite split the others do —
/// every DSF write is already the cheap kind.
///
/// `.dff` is still refused. It has no ID3 by convention, its metadata
/// lives in its own chunk structure, and some taggers append an ID3
/// chunk anyway — so "which shape do we write" is a real question with
/// no obvious answer, and half an answer would be worse than today's
/// clear no.
pub(super) fn patch_dsf(
    path: &std::path::Path,
    patch: &TagPatch<'_>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    with_dsf_tag(path, |tag, version| {
        apply_patch_id3(tag, patch);
        mirror_year_for_legacy(tag, patch, version);
        join_genres_for_legacy(tag, version);
        // An edit the dialog sent is an edit to write, even when it
        // happens to set what the file already said: the alternative is
        // comparing two encodings of the same tag to save a write of a
        // block that sits after the audio and costs nothing to move.
        true
    })
}

/// Read a DSF file's ID3v2 tag, let `edit` change it, write it back.
///
/// The read-modify-write half of [`patch_dsf`], on its own because it
/// has a second caller: the canonical `SYNCEDLYRICS` stamp
/// (`commands::lyrics`) is a different payload over the same container,
/// and a second copy of this would be a second place to get the
/// preservation rules wrong.
///
/// `edit` is handed the version the file arrived with, for the one
/// decision that depends on it (`TYER` versus `TDRC`), and answers
/// whether the tag should be written back. `false` leaves the file
/// untouched — for the callers whose edit may turn out to be no edit at
/// all, where rewriting would cost the user a file modification, and a
/// re-hash, for nothing.
pub(crate) fn with_dsf_tag<F>(
    path: &std::path::Path,
    edit: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    F: FnOnce(&mut id3::Tag, id3::Version) -> bool,
{
    waveflow_core::tagio::with_writable_file(
        path,
        |handle| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let layout = waveflow_core::tagio::read_dsf_layout(handle)?
                .ok_or("this DSF file's header does not describe itself")?;
            let existing = if layout.metadata_offset != 0 {
                use std::io::{Seek, SeekFrom};
                handle.seek(SeekFrom::Start(layout.metadata_offset))?;
                // A tag we cannot parse stops the edit. Carrying on with
                // a fresh one looks like resilience and is the opposite:
                // the write replaces the whole tag, so every frame we
                // failed to read — the cover, the ratings, the
                // MusicBrainz identifiers — would be dropped to save a
                // title. Refusing leaves the file exactly as it is, and
                // says why.
                //
                // `NoTag` is the exception, and the only one: a pointer
                // leading to no tag at all describes a file with no
                // metadata, not one whose metadata we failed to read.
                // There is nothing there to lose, so it starts fresh.
                match id3::Tag::read_from2(&mut *handle) {
                    Ok(tag) => Some(tag),
                    Err(err) if matches!(err.kind, id3::ErrorKind::NoTag) => None,
                    Err(err) => return Err(err.into()),
                }
            } else {
                None
            };
            // Write back the version the file already used. A DSF tagged
            // as ID3v2.3 rewritten as 2.4 is a file some hardware DSD
            // players stop reading, and an edit to a title is not the
            // place to make that decision for the user. Only a tag we
            // are creating gets our own choice.
            let version = existing
                .as_ref()
                .map_or(id3::Version::Id3v24, id3::Tag::version);
            let mut tag = existing.unwrap_or_default();
            if !edit(&mut tag, version) {
                return Ok(());
            }
            let mut bytes = Vec::new();
            tag.write_to(&mut bytes, version)?;
            waveflow_core::tagio::write_dsf_id3v2(handle, &bytes)?;
            Ok(())
        },
    )
}

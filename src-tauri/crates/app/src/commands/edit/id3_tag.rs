//! The patch applied to a concrete ID3v2 tag, and what the legacy ID3
//! versions need on top of it.

use waveflow_core::scanner::split_genres;

use super::*;

/// Apply a patch to an [`id3::Tag`], for the one container lofty cannot
/// write (#592).
///
/// The mirror of [`apply_patch`], deliberately kept beside it. DSF keeps
/// its metadata as a plain ID3v2 tag at an offset its header declares,
/// and lofty has no `FileType` for it at all — so the tag has to be
/// parsed and re-encoded by the `id3` crate the DSD reader already uses.
/// Two tag models, one set of edit semantics: a test asserts the two
/// appliers agree field by field, because a divergence here would mean
/// the same edit meaning different things depending on the container.
///
/// Frames this does not name are carried through untouched, which is
/// what `id3` does by holding the frames it parsed.
pub(super) fn apply_patch_id3(tag: &mut id3::Tag, patch: &TagPatch<'_>) {
    use id3::TagLike;

    let edit = match patch {
        TagPatch::Fields(edit) => edit,
        TagPatch::Rating(rating) => {
            // `id3` models POPM, so the frame is built from its fields
            // rather than from a hand-packed body — same bytes, and the
            // crate owns the encoding. Removing first covers the frames
            // another tagger wrote under its own email: POPM is keyed by
            // user, so several can sit in one tag and the reader takes
            // the first.
            tag.remove("POPM");
            if let Some(r) = rating {
                tag.add_frame(id3::Frame::with_content(
                    "POPM",
                    id3::Content::Popularimeter(id3::frame::Popularimeter {
                        user: String::new(),
                        rating: *r,
                        counter: 0,
                    }),
                ));
            }
            return;
        }
        TagPatch::Lyrics(slot) => {
            match slot {
                LyricsSlot::Cleared => tag.remove_all_lyrics(),
                LyricsSlot::Unsynchronised(text) => {
                    tag.remove_all_lyrics();
                    tag.add_frame(id3::frame::Lyrics {
                        // `und` — undetermined. USLT keys on the
                        // language, and claiming one we have not
                        // detected would let a reader hide the lyrics
                        // from someone whose player filters by it.
                        lang: "und".to_string(),
                        description: String::new(),
                        text: (*text).to_string(),
                    });
                }
                // ID3v2 has no arbitrary-string lyrics slot, so
                // `write_lyrics_to_file` refuses TTML on every ID3v2
                // container before it reaches an applier. This arm is
                // what happens if that guard is ever moved: leave the
                // tag exactly as it is. Clearing it would spend the
                // lyrics already in the file on a write we cannot make.
                LyricsSlot::Arbitrary(_) => {}
            }
            return;
        }
        TagPatch::Cover { bytes, mime } => {
            // Replace the cover, not the artwork — the same rule, and
            // the same reason: a release with a booklet, a back cover or
            // an artist shot carries several pictures and nothing brings
            // them back. `id3` removes pictures all at once, so the ones
            // that survive are put back rather than left alone.
            let kept: Vec<id3::frame::Picture> = tag
                .pictures()
                .filter(|picture| {
                    !matches!(
                        picture.picture_type,
                        id3::frame::PictureType::CoverFront | id3::frame::PictureType::Other
                    )
                })
                .cloned()
                .collect();
            tag.remove_all_pictures();
            for picture in kept {
                tag.add_frame(picture);
            }
            tag.add_frame(id3::frame::Picture {
                mime_type: mime.to_string(),
                picture_type: id3::frame::PictureType::CoverFront,
                description: String::new(),
                data: bytes.to_vec(),
            });
            return;
        }
    };

    if let Some(t) = edit.title.as_ref() {
        if t.trim().is_empty() {
            tag.remove_title();
        } else {
            tag.set_title(t.trim());
        }
    }
    if let Some(a) = edit.artist.as_ref() {
        if a.trim().is_empty() {
            tag.remove_artist();
        } else {
            tag.set_artist(a.trim());
        }
    }
    if let Some(al) = edit.album.as_ref() {
        if al.trim().is_empty() {
            tag.remove_album();
        } else {
            tag.set_album(al.trim());
        }
    }
    if let Some(y) = edit.year {
        // The date item, not the bare year, for the same reason the
        // lofty side uses it: that is what the scanner reads back. The
        // legacy `TYER` is cleared alongside so a stale value cannot
        // outlive the one the user just removed.
        if y <= 0 {
            tag.remove_date_recorded();
            tag.remove_year();
        } else if let Ok(year) = i32::try_from(y) {
            match tag.date_recorded() {
                // A save that changed only the title must not truncate a
                // full release date to its year.
                Some(existing) if existing.year == year => {}
                _ => tag.set_date_recorded(id3::Timestamp {
                    year,
                    ..Default::default()
                }),
            }
        }
    }
    if let Some(n) = edit.track_number {
        if n > 0 {
            tag.set_track(n as u32);
        } else {
            tag.remove_track();
        }
    }
    if let Some(n) = edit.disc_number {
        if n > 0 {
            tag.set_disc(n as u32);
        } else {
            tag.remove_disc();
        }
    }
    if let Some(g) = edit.genre.as_ref() {
        let names = split_genres([g.as_str()]);
        if names.is_empty() {
            tag.remove_genre();
        } else {
            // One TCON, its values NUL-separated, as the lofty side writes.
            tag.set_text_values("TCON", names);
        }
    }
    if let Some(aa) = edit.album_artist.as_ref() {
        if aa.trim().is_empty() {
            tag.remove_album_artist();
        } else {
            tag.set_album_artist(aa.trim());
        }
    }
    if let Some(c) = edit.composer.as_ref() {
        if c.trim().is_empty() {
            tag.remove("TCOM");
        } else {
            tag.set_text("TCOM", c.trim());
        }
    }
}

/// Put the year where a pre-2.4 reader will look for it.
///
/// `TDRC` is a 2.4 frame. The `id3` crate writes whatever frames the tag
/// holds, version target or not, so a 2.3 tag built from a `TDRC` comes
/// back out carrying `TDRC` — and a reader that only knows 2.3 finds no
/// year at all (measured: `year()` answers `None` after that round
/// trip). Since a DSF now keeps the version it arrived with, the year
/// has to be mirrored into `TYER`, which is the frame the older
/// versions define — 2.2 among them, where the writer shortens it to
/// `TYE` on the way out.
///
/// A cleared year clears both, so a stale `TYER` cannot outlive the
/// date it duplicated.
///
/// **Only when the edit touched the year.** A genuine 2.3 tag carries
/// `TYER` and no `TDRC` at all — that is the normal shape, not an odd
/// one — so a mirror that ran on every save read "no date" from a file
/// that had a perfectly good year and removed it. Editing a title, or
/// setting a cover, would have quietly erased the year of every 2.3 DSF
/// it touched.
/// Keep several genres readable as several in an ID3v2.2 / 2.3 tag
/// (#729).
///
/// Only 2.4 has multi-value text frames. Asked to write one into an older
/// version, the `id3` crate joins the values with `/` (measured: "Jazz" and
/// "Trip Hop" come back as "Jazz/Trip Hop"), and `split_genres` does not
/// split on `/`, which belongs to names like "Hip-Hop/Rap". So an older
/// tag gets the one value the scanner does split: the genres joined with
/// `"; "`.
pub(super) fn join_genres_for_legacy(tag: &mut id3::Tag, version: id3::Version) {
    use id3::TagLike;

    if !matches!(version, id3::Version::Id3v22 | id3::Version::Id3v23) {
        return;
    }
    let Some(values) = tag.genres() else {
        return;
    };
    if values.len() < 2 {
        return;
    }
    let joined = values.join("; ");
    tag.set_genre(joined);
}

pub(super) fn mirror_year_for_legacy(
    tag: &mut id3::Tag,
    patch: &TagPatch<'_>,
    version: id3::Version,
) {
    use id3::TagLike;

    if !matches!(version, id3::Version::Id3v22 | id3::Version::Id3v23) {
        return;
    }
    // Only a field edit can carry a year at all — a cover, a rating and
    // a set of lyrics do not — and a field edit that left `year` at
    // `None` is asking for the year to stay exactly as it is.
    let TagPatch::Fields(edit) = patch else {
        return;
    };
    if edit.year.is_none() {
        return;
    }
    match tag.date_recorded() {
        Some(date) => {
            tag.set_year(date.year);
            // 2.2 frame IDs are three characters, and `TDRC` has no
            // three-character form — so the writer does not drop it, it
            // refuses the whole tag: "Unable to downgrade frame ID to
            // ID3v2.2" (measured). Editing the year of a 2.2 file failed
            // outright because of a frame we had just added. `TYER`
            // carries the year now, so the frame goes back out.
            if version == id3::Version::Id3v22 {
                tag.remove_date_recorded();
            }
        }
        None => tag.remove_year(),
    }
}

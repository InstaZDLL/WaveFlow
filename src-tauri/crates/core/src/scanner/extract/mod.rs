//! Pure-Rust file extractors used by the scanner: hash, cover, artist
//! image, rating, musical key, tag-to-struct mapping.
//!
//! Everything here is filesystem + lofty + image; no SQL, no Tauri.
//! The orchestrator (`scan_folder_inner` in `crates/app`) calls these
//! helpers per file and then hands the resulting [`ExtractedFile`] to
//! the [`super::upserts`] family for the DB writes.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use lofty::file::FileType;
use lofty::tag::{ItemKey, Tag, TagType};

mod artist_images;
mod artwork;
mod probe;

pub use artist_images::{
    extract_artist_image, extract_artist_image_cached, find_artist_image_in_dir,
    write_artist_image, ArtistImageDirCache, DirImageCandidate,
};
pub use artwork::{
    cover_extension, extension_for_mime, extract_cover, extract_folder_cover, pick_folder_cover,
    store_folder_cover, ArtworkSource, ExtractedCover, FOLDER_COVER_EXTENSIONS,
};
pub use probe::{
    decoder_channels, decoder_channels_from, decoder_decides, probe_playable_source,
    probe_playable_stream, StreamInfo,
};

/// Extensions considered "audio files" by the scanner. Limited to
/// formats the symphonia + cpal engine can actually decode and play,
/// so the library never displays tracks that would error at play time.
///
/// What is absent, and why:
///
/// - **WMA** is genuinely proprietary: Microsoft, unpublished
///   specification, patent-encumbered. This one stays out, and it is
///   the only entry here that is a licence question.
/// - **AIFF** is not. It is a container, not a codec — Apple's answer
///   to WAV, holding the same PCM — and symphonia reads it from
///   `symphonia-format-riff`, the crate `wav` already pulls in.
///
/// **Opus** used to be absent, for a purely technical reason:
/// symphonia ships no Opus decoder. We do now — `libopus` through
/// [`crate::audio_format::opus`], the way DSD is handled below — so
/// the extension is accepted and [`undecodable_stream`] no longer
/// refuses the stream. The ambiguity that outlived it is still real
/// and still handled: `.ogg` names a container, not a codec, so
/// [`is_scannable_audio`] reads the stream before believing the
/// extension.
///
/// **WavPack** and **Monkey's Audio** have decoders of ours as well, each
/// with a family of files it does not play (hybrid WavPack, floating-point
/// Monkey's Audio and a few more): [`is_scannable_audio`] reads the header of
/// these two before believing the extension.
///
/// `.aifc` is deliberately not listed. symphonia's AIFC support covers
/// the PCM-shaped compression types (`none`, `sowt`, `twos`, `fl32`,
/// `alaw`, …) and refuses the rest, so accepting the extension would
/// index files that cannot play, on a format nobody writes any more.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "aiff", "aif", "ogg", "oga", "opus", "m4a", "mp4", "aac", "wv", "ape",
    // DSD: handled by the in-tree audio::dsd pipeline (symphonia
    // doesn't decode DSD), with metadata read via audio::dsd::metadata.
    "dsf", "dff",
];

/// Containers whose extension names the box and not the stream inside
/// it. Everything else in [`AUDIO_EXTENSIONS`] implies its codec closely
/// enough that the extension is the whole answer; these do not, so they
/// pay for one header read before the scanner believes them.
const AMBIGUOUS_CONTAINERS: &[&str] = &["ogg", "oga"];

/// Why the local engine cannot decode this container's stream, or `None`
/// when it can.
///
/// Stated as a deny list rather than an allow list on purpose. An allow
/// list that forgot a codec would drop files the engine plays perfectly
/// well — including anything lofty reports as `Custom`, and any codec a
/// future symphonia feature adds. Every entry here is a stream lofty
/// identifies and this build ships no decoder for.
pub fn undecodable_stream(file_type: &FileType) -> Option<&'static str> {
    match file_type {
        // Opus came off this list when the decoder landed (#581).
        FileType::Speex => Some("Speex"),
        _ => None,
    }
}

/// Whether the scanner should index this path at all.
///
/// The extension check alone was wrong for one family of files: `.ogg`
/// and `.oga` name a container, and Opus travels in the same one Vorbis
/// does. Such a file passed a list built for Vorbis, was indexed, and
/// then failed at play time — the single thing [`AUDIO_EXTENSIONS`]
/// exists to prevent. So an ambiguous container is opened far enough to
/// read which stream it actually carries.
///
/// A file that stops qualifying is not deleted from a library that
/// already holds it: it simply stops being walked, and the scan's
/// disappearance sweep marks the row unavailable, keeping its likes,
/// playlists and play history. Ship a decoder for it later and the next
/// scan brings the same row back.
pub fn is_scannable_audio(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    let extension = extension.to_lowercase();
    if !AUDIO_EXTENSIONS.contains(&extension.as_str()) {
        return false;
    }
    let refusal = match extension.as_str() {
        "wv" => crate::audio_format::wavpack::refusal(path),
        "ape" => crate::audio_format::ape::refusal(path),
        _ => None,
    };
    if let Some(codec) = refusal {
        tracing::info!(
            path = %path.display(),
            codec,
            "this build cannot decode the stream; not indexing it"
        );
        return false;
    }
    if !AMBIGUOUS_CONTAINERS.contains(&extension.as_str()) {
        return true;
    }
    // A container we cannot read is left to the extractor to report:
    // refusing it here would turn an unreadable file into an absent one.
    let Some(file_type) = lofty::probe::Probe::open(path)
        .ok()
        .and_then(|probe| probe.guess_file_type().ok())
        .and_then(|probe| probe.file_type())
    else {
        return true;
    };
    match undecodable_stream(&file_type) {
        Some(codec) => {
            tracing::info!(
                path = %path.display(),
                codec,
                "container holds a stream this build cannot decode; not indexing it"
            );
            false
        }
        None => true,
    }
}

/// Bytes hashed from each of the file's head and tail in the partial
/// path. Public for the network scan, which fetches these two windows in one
/// request each and serves the tag read from them. 1 MiB each — large enough that distinct tracks differ inside
/// the window (leading frames) and that tag rewrites land in it (ID3v2
/// at the head, ID3v1 / APE / Lyrics3 at the tail), small enough that a
/// multi-MB track reads ~2 MiB instead of its whole length.
pub const HASH_CHUNK_BYTES: u64 = 1024 * 1024;

/// Content hash used for dedup + tag-edit detection.
///
/// Files larger than `2 * HASH_CHUNK_BYTES` are hashed over their size +
/// first chunk + last chunk only, instead of every byte. For a typical
/// audio library this cuts the scan's disk I/O several-fold (full-file
/// hashing was the dominant cost — reading ~9 GB to scan 900 tracks)
/// while staying a strong identity:
/// - moved / renamed files keep the same bytes → same hash (dedup holds),
/// - a tag rewrite shifts bytes in the head/tail window → hash changes,
///   so the scanner still re-extracts edited files,
/// - the file length is folded in, so two files sharing head+tail but
///   differing in size never collide.
///
/// Blind spot: two *distinct* files with identical size, head and tail
/// but different middle bytes would collide. For real music that does
/// not occur. Smaller files (≤ `2 * HASH_CHUNK_BYTES`) are hashed whole.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    hash_reader(file, len)
}

/// [`hash_file`] over any reader of `len` bytes: the same digest for the same
/// bytes, so a file on a share and its copy on this disk are recognised as
/// one (duplicates, the lyrics cache, an orphaned history finding its track).
pub fn hash_reader<R: Read + Seek>(mut file: R, len: u64) -> std::io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&len.to_le_bytes());

    if len <= 2 * HASH_CHUNK_BYTES {
        // Small enough to read fully — also the path most callers and
        // unit tests exercise.
        let mut reader = std::io::BufReader::new(file);
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    } else {
        let chunk = HASH_CHUNK_BYTES as usize;
        let mut head = vec![0u8; chunk];
        file.read_exact(&mut head)?;
        hasher.update(&head);

        let mut tail = vec![0u8; chunk];
        file.seek(SeekFrom::End(-(HASH_CHUNK_BYTES as i64)))?;
        file.read_exact(&mut tail)?;
        hasher.update(&tail);
    }

    Ok(hasher.finalize().to_hex().to_string())
}

/// Full-content BLAKE3 hash — every byte. Slower than [`hash_file`]
/// (which reads only the head + tail of large files), so it's NOT used
/// on the hot scan path. The duplicate-detection flow calls it to
/// confirm that tracks sharing the cheap partial digest are *really*
/// byte-identical before offering to delete one — closing the partial
/// hash's middle-byte blind spot for that destructive path.
pub fn hash_file_full(path: &Path) -> std::io::Result<String> {
    let file = fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Everything the scanner reads off disk for a single audio file. Populated
/// inside `spawn_blocking` so the tokio reactor never stalls on I/O.
pub struct ExtractedFile {
    pub abs_path: String,
    pub size: i64,
    pub modified_ms: i64,
    pub hash: String,
    pub title: String,
    /// `title` is the file's own name, because the tag had none.
    ///
    /// The fallback is right for display -- an empty row in the library
    /// grid helps nobody -- but it erases the difference between a file
    /// that is titled and one that is not, and that difference is
    /// exactly what the "needs attention" inventory is asked for
    /// (#589). Recorded here rather than recovered later: comparing the
    /// stored title against the path in SQL would report every single
    /// correctly tagged `Song.mp3` as untitled.
    pub title_from_filename: bool,
    pub artist: Option<String>,
    /// The MusicBrainz id the file gives each of `artist`'s names, in the
    /// tag's order (see [`super::canonical::artist_credits`] for how they pair).
    pub artist_mbids: Vec<String>,
    pub album: Option<String>,
    /// Raw Album Artist text from the source tag (`TPE2` / `aART` /
    /// `ALBUMARTIST` / `Album Artist`). Used as the album-grouping
    /// authority — when present, two tracks share an album even if
    /// their per-track Artist tags differ (featurings, lead-vocal
    /// rotations on K-pop EPs, etc.).
    pub album_artist: Option<String>,
    /// The MusicBrainz id of `album_artist`, when the tag names exactly one.
    pub album_artist_mbid: Option<String>,
    /// `TCMP` (ID3v2) / `cpil` (MP4) / `COMPILATION` (Vorbis / APE)
    /// flag. When `true` the scanner uses a synthetic "Various
    /// Artists" album artist so a true compilation merges its tracks
    /// under a single album row even when no Album Artist tag exists.
    pub is_compilation: bool,
    /// Every genre the tag names, split and deduplicated by
    /// [`split_genres`](super::canonical::split_genres), in tag order.
    pub genres: Vec<String>,
    pub year: Option<i64>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    /// The track total the tag carries (`TRCK` "4/12", `TRACKTOTAL`,
    /// `trkn`) — the size of the track's disc. Read for the album
    /// completeness check, which cannot see a missing last track without
    /// it.
    pub track_total: Option<i64>,
    /// The disc total (`TPOS` "1/2", `DISCTOTAL`, `disk`).
    pub disc_total: Option<i64>,
    pub duration_ms: i64,
    pub bitrate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    /// Bits per sample (16 for CD-quality, 24/32 for Hi-Res masters).
    /// Lossy codecs (MP3, AAC) typically don't expose this — left as
    /// `None` so the UI's Hi-Res badge logic can short-circuit without
    /// inspecting the codec separately.
    pub bit_depth: Option<i64>,
    /// Short codec / container label inferred from the file type
    /// (e.g. `"FLAC"`, `"MP3"`, `"AAC"`, `"WAV"`). Drives the format
    /// chip on the player footer.
    pub codec: Option<String>,
    /// Tagged musical key when the file carries one (`TKEY` / ID3v2
    /// or `INITIALKEY` / Vorbis-MP4-APE). Whatever notation the
    /// upstream tagger chose stays as-is — could be `Am`, `F#`, or
    /// the Camelot wheel `8A`.
    pub musical_key: Option<String>,
    /// Embedded cover art extracted and hash-addressed during the scan. One
    /// picture is kept: the front cover wherever it sits, or else the first
    /// one with any bytes ([`artwork::extract_cover`]). `None` when the tag
    /// has no non-empty picture.
    pub cover_art: Option<ExtractedCover>,
    /// Raw POPM byte (0-255) for ID3v2 files, or a normalised value
    /// derived from the `RATING` text field for Vorbis/FLAC/MP4. `None`
    /// when neither tag carries a rating.
    pub rating: Option<u8>,
    /// ReplayGain the file already carries, normalised to the
    /// ReplayGain 2.0 scale. Refreshed on every scan because an
    /// external tagger can add or recompute it at any time; playback
    /// prefers it over our own analysis, which is why it is read here
    /// rather than only when the user asks for an analysis pass.
    pub replay_gain: super::replay_gain::ReplayGainTags,
    /// Tags the file carries that the generic `Tag` cannot model, as
    /// `(key, value)` sorted by key (#588). Read from the concrete
    /// container by [`super::extra_tags`].
    ///
    /// `None` means the second parse could not read them — which is not
    /// the same as the file having none, and the scanner must not treat
    /// it as such: the write is delete-then-insert, so a failed read
    /// folded into "no tags" would erase what is stored.
    pub extra_tags: Option<Vec<(String, String)>>,
}

/// Map lofty's `FileType` enum to a short uppercase label suitable
/// for the UI's format chip. Falls back to `None` when lofty can't
/// determine a recognized container — we'd rather hide the chip
/// than print "Unknown".
pub fn file_type_label(ft: FileType) -> Option<String> {
    match ft {
        FileType::Mpeg => Some("MP3".into()),
        FileType::Flac => Some("FLAC".into()),
        FileType::Mp4 => Some("AAC".into()),
        FileType::Aac => Some("AAC".into()),
        FileType::Wav => Some("WAV".into()),
        FileType::Vorbis => Some("Vorbis".into()),
        FileType::Opus => Some("Opus".into()),
        FileType::Aiff => Some("AIFF".into()),
        FileType::Speex => Some("Speex".into()),
        FileType::Ape => Some("APE".into()),
        FileType::WavPack => Some("WavPack".into()),
        FileType::Custom(name) => Some(name.to_string()),
        _ => None,
    }
}

/// Extract a 0-255 rating from a tag.
///
/// Three shapes reach this, and the middle one is new. **lofty 0.25
/// changed what a POPM frame looks like on the generic tag**: it used
/// to arrive as a raw `ItemValue::Binary` holding the frame body, and
/// it now arrives as the text `<provider>|<stars>|<counter>`, converted
/// through a whole-star `StarRating` (lofty's own doc comment still
/// describes the old shape). Reading only the first two is why no
/// rating has been read out of an MP3 since that bump: `get_binary`
/// answered `None`, and `"someone@example.com|4|0"` is not a number.
///
/// The star form is rescaled on the same 51-per-star scale the app
/// writes, so four stars stay four stars. It is lossy in one direction
/// and unavoidably so: the byte another tagger wrote is mapped to a
/// star by lofty before we ever see it, and half-stars have no
/// representation there at all. That is why a scan does not stop here for
/// a frame: [`rating_from_popm`] reads them on the concrete tag, for the
/// byte, and for the zero lofty would turn into one star.
///
/// A zero answers `None`: it is no star the app can set. Read as a rating,
/// it would replace the one the library holds, which a file saying nothing
/// leaves alone.
pub fn extract_rating(tag: &Tag) -> Option<u8> {
    read_rating(tag).filter(|rating| *rating > 0)
}

fn read_rating(tag: &Tag) -> Option<u8> {
    if matches!(tag.tag_type(), TagType::Id3v2) {
        if let Some(bytes) = tag.get_binary(ItemKey::Popularimeter, false) {
            let nul_pos = bytes.iter().position(|b| *b == 0)?;
            return bytes.get(nul_pos + 1).copied();
        }
    }
    if let Some(text) = tag.get_string(ItemKey::Popularimeter) {
        let trimmed = text.trim();
        // The generic popularimeter, and the whole shape of it: a
        // provider name that may be empty, the stars, the play counter.
        // Matched exactly rather than by position, because this value
        // was written by software we know nothing about — a string that
        // merely happens to carry a pipe and a digit is not a rating,
        // and reading it as one puts stars on a track nobody rated.
        // Tried before the plain number, which has no pipe at all.
        let fields: Vec<&str> = trimmed.split('|').collect();
        if let [_provider, stars, counter] = fields.as_slice() {
            if counter.trim().parse::<u64>().is_ok() {
                if let Ok(stars) = stars.trim().parse::<u16>() {
                    if (1..=5).contains(&stars) {
                        return Some((stars * 51) as u8);
                    }
                }
            }
        }
        if let Some(rating) = rating_from_number(trimmed) {
            return Some(rating);
        }
    }
    None
}

/// A rating written as a plain number, as a 0-255 byte. `None` for a zero,
/// which is no rating, and for anything that is not a number.
///
/// Most software writes this on a 0-100 scale, and so does this app. Some
/// players write 1 to 5 instead. Those are read as stars: on the other
/// scale they would be a fraction of one star, which no widget produces
/// (half a star is 10).
pub fn rating_from_number(text: &str) -> Option<u8> {
    match text.trim().parse::<u16>().ok()? {
        0 => None,
        stars @ 1..=5 => Some((stars * 51) as u8),
        percent => {
            // Rounded to the nearest byte, not truncated. The app writes
            // this scale as `byte * 100 / 255`, so the two halves have
            // to meet: truncating sent 128 back as 127. Every value the
            // star widget produces — whole stars and halves, 26 through
            // 255 — survives the round trip exactly.
            Some(((percent.min(100) * 255 + 50) / 100) as u8)
        }
    }
}

/// The rating the POPM frames of an ID3v2 tag hold, given as `(email, byte)`.
/// The outer `None` means there is no frame, so the frames have no say.
///
/// A frame with no email is the one this app writes, and the only one it has
/// ever written: its byte is the rating, on the app's own scale, half stars
/// included. Lofty's generic tag cannot be asked for it, since it rounds
/// every frame to a whole star on a scale of its own first (204, four stars,
/// comes back as five).
///
/// A frame with an email belongs to another program, and there the rounding
/// is what is wanted: there is no agreed scale, some write 1 for one star,
/// and that byte taken as it is would show as no star at all. So it goes
/// through lofty, which knows the scales of the emails it knows.
///
/// A zero is ID3's own word for unknown, and often sits beside a play count.
pub fn rating_from_popm<'a>(frames: impl Iterator<Item = (&'a str, u8)>) -> Option<Option<u8>> {
    let frames: Vec<(&str, u8)> = frames.collect();
    if frames.is_empty() {
        return None;
    }
    let rated = || frames.iter().filter(|(_, byte)| *byte > 0);
    if let Some((_, byte)) = rated().find(|(email, _)| email.is_empty()) {
        return Some(Some(*byte));
    }
    Some(rated().next().and_then(|(email, byte)| {
        use lofty::id3::v2::{Frame, Id3v2Tag, PopularimeterFrame};
        use lofty::tag::SplitTag;
        let mut tag = Id3v2Tag::default();
        tag.insert(Frame::Popularimeter(PopularimeterFrame::new(
            (*email).to_string(),
            *byte,
            0,
        )));
        extract_rating(&tag.split_tag().1)
    }))
}

/// The key a rating goes under in an APEv2 tag.
pub const APE_RATING_KEY: &str = "RATING";

/// Copy into `tagged`'s generic tag what lofty leaves out of it for the APEv2
/// tag of a WavPack or Monkey's Audio file: the rating, for which its key
/// table has no entry, and the pictures, which it keeps as binary items for
/// the caller to decode. Every reader goes through the generic tag, so without
/// these a cover in the file is never shown and a scan overwrites
/// `track.rating` with nothing.
fn complete_ape_tag(path: &Path, tagged: &mut lofty::file::TaggedFile) {
    use lofty::config::ParseOptions;
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::picture::{Picture, PictureType, APE_PICTURE_TYPES};

    let file_type = tagged.file_type();
    if !matches!(file_type, FileType::Ape | FileType::WavPack) {
        return;
    }
    let Some(tag) = tagged.primary_tag_mut() else {
        return;
    };
    let Ok(mut file) = fs::File::open(path) else {
        return;
    };
    let options = ParseOptions::new().read_properties(false);
    let concrete = match file_type {
        FileType::Ape => lofty::ape::ApeFile::read_from(&mut file, options)
            .ok()
            .and_then(|f| f.ape().cloned()),
        _ => lofty::wavpack::WavPackFile::read_from(&mut file, options)
            .ok()
            .and_then(|f| f.ape().cloned()),
    };
    let Some(concrete) = concrete else {
        return;
    };

    if let Some(lofty::tag::ItemValue::Text(rating)) =
        concrete.get(APE_RATING_KEY).map(|item| item.value())
    {
        // Unchecked because of that same table: a checked insert drops it.
        tag.insert_unchecked(lofty::tag::TagItem::new(
            ItemKey::Popularimeter,
            lofty::tag::ItemValue::Text(rating.clone()),
        ));
    }
    let mut pictures: Vec<Picture> = APE_PICTURE_TYPES
        .iter()
        .filter_map(|key| match concrete.get(key)?.value() {
            lofty::tag::ItemValue::Binary(bytes) => Picture::from_ape_bytes(key, bytes).ok(),
            _ => None,
        })
        .collect();
    // The front cover is the one a reader takes first.
    pictures.sort_by_key(|picture| picture.pic_type() != PictureType::CoverFront);
    for picture in pictures {
        tag.push_picture(picture);
    }
}

/// Read the tagged musical key, if any. ID3v2 stores it as `TKEY`,
/// Vorbis comments / MP4 / APE / WavPack as `INITIALKEY` — lofty
/// unifies both behind `ItemKey::InitialKey`. Empty strings are
/// coalesced to `None` so the UI's "—" placeholder kicks in
/// instead of a blank cell.
pub fn extract_musical_key(tag: &Tag) -> Option<String> {
    let raw = tag.get_string(ItemKey::InitialKey)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Pull the Album Artist tag and trim it. Lofty's `ItemKey::AlbumArtist`
/// already abstracts the per-container mapping (`TPE2` / `aART` /
/// `ALBUMARTIST` / `Album Artist`). Empty / whitespace-only strings are
/// treated as missing so the grouping code falls back to the per-track
/// Artist exactly like before.
pub fn extract_album_artist(tag: &Tag) -> Option<String> {
    let raw = tag.get_string(ItemKey::AlbumArtist)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// The credit the tag gives a track: see [`artist_credit`]. Every value, not
/// `Accessor::artist`, which is the first one only. lofty already turns a
/// multi-value `TPE1`, repeated Vorbis `ARTIST` comments and several `©ART`
/// into one item each, and maps `ARTISTS` from whichever frame or atom the
/// container keeps it in. Two names that differ by case only stay two when
/// the tag gives each its own MusicBrainz id ([`extract_artist_credit`]).
pub fn extract_artists(tag: &Tag) -> Option<String> {
    extract_artist_credit(tag).0
}

/// The credit the tag gives a track and the MusicBrainz artist ids that go
/// with it (`MUSICBRAINZ_ARTISTID`, `TXXX:MusicBrainz Artist Id`, the MP4
/// atom of the same name): the identity that tells two artists of one
/// spelling apart (#1076). The ids are those of
/// [`super::canonical::artist_mbids`], aligned with the credit's names when
/// they pair with the names as written: a credit "LiSA; LISA" with two ids
/// is two artists (#1081, [`super::canonical::artist_credit_with_ids`]).
pub fn extract_artist_credit(tag: &Tag) -> (Option<String>, Vec<String>) {
    super::canonical::artist_credit_with_ids(
        tag.get_strings(ItemKey::TrackArtists),
        tag.get_strings(ItemKey::TrackArtist),
        &extract_artist_mbids(tag),
    )
}

/// The MusicBrainz artist ids the tag carries for the track's artists, as
/// they are written: see [`extract_artist_credit`] for the ones that go with
/// the credit.
pub fn extract_artist_mbids(tag: &Tag) -> Vec<String> {
    super::canonical::artist_mbids(tag.get_strings(ItemKey::MusicBrainzArtistId))
}

/// The MusicBrainz id of the Album Artist, when the tag names exactly one
/// id *and* the Album Artist is one name: an id for "Alpha; Beta" would
/// be Alpha's, not the pair's. See [`super::canonical::album_artist_mbid`].
///
/// Every value of the Album Artist tag is counted, not the first one
/// [`extract_album_artist`] groups by: two Vorbis `ALBUMARTIST` comments,
/// "Alpha" and "Beta", are two names, and one id says nothing of whose.
pub fn extract_album_artist_mbid(tag: &Tag) -> Option<String> {
    let names: Vec<&str> = tag
        .get_strings(ItemKey::AlbumArtist)
        .flat_map(|v| v.split('\0'))
        .collect();
    super::canonical::album_artist_mbid(
        &names.join("; "),
        tag.get_strings(ItemKey::MusicBrainzReleaseArtistId),
    )
}

/// Read the compilation flag (`TCMP` / `cpil` / `COMPILATION` / `Compilation`).
/// Lofty stores the value as a stringified `0` / `1` regardless of the
/// underlying container; anything that parses to a non-zero integer or the
/// literal `true` is treated as "this is a compilation".
pub fn extract_compilation_flag(tag: &Tag) -> bool {
    let Some(raw) = tag.get_string(ItemKey::FlagCompilation) else {
        return false;
    };
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("true") {
        return true;
    }
    matches!(trimmed.parse::<i64>(), Ok(n) if n != 0)
}

/// Read a file's tags and properties with lofty, judging the format by
/// what the file contains rather than what it is called (#873).
///
/// `lofty::read_from_path` trusts the extension: an AAC or M4A saved as
/// `.mp3`, which download sites hand out, was parsed as MPEG and refused
/// ("failed to parse Mpeg file"), so the scanner left out a file the
/// player would have played. Sniffing the content reads it as what it
/// is. A parse that still fails is retried once in `Relaxed` mode, which
/// steps over malformed tag frames instead of giving up on the file; the
/// first error is the one reported when both fail.
pub fn read_tagged_file(path: &Path) -> Result<lofty::file::TaggedFile, String> {
    use lofty::config::{ParseOptions, ParsingMode};
    use lofty::probe::Probe;

    crate::tagio::chapter_nesting::check_file(path).map_err(|e| e.to_string())?;
    let read = |mode: ParsingMode| -> Result<lofty::file::TaggedFile, String> {
        Probe::open(path)
            .map_err(|e| error_chain(&e))?
            .options(ParseOptions::new().parsing_mode(mode))
            .guess_file_type()
            .map_err(|e| error_chain(&e))?
            .read()
            .map_err(|e| error_chain(&e))
    };
    let mut tagged = read(ParsingMode::BestAttempt)
        .or_else(|first| read(ParsingMode::Relaxed).map_err(|_| first))?;
    complete_ape_tag(path, &mut tagged);
    Ok(tagged)
}

/// [`read_tagged_file`] over any reader, for a file that is not on this disk.
/// `hint` stands in for the extension `Probe::open` would have read, and only
/// counts when the content does not settle the format.
pub fn read_tagged_from<R: Read + Seek>(
    reader: &mut R,
    hint: Option<FileType>,
) -> Result<lofty::file::TaggedFile, String> {
    use lofty::config::{ParseOptions, ParsingMode};
    use lofty::probe::Probe;

    reader
        .seek(SeekFrom::Start(0))
        .map_err(|e| error_chain(&e))?;
    crate::tagio::chapter_nesting::check_reader(reader).map_err(|e| e.to_string())?;
    let mut read = |mode: ParsingMode| -> Result<lofty::file::TaggedFile, String> {
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|e| error_chain(&e))?;
        let mut probe = Probe::new(&mut *reader).options(ParseOptions::new().parsing_mode(mode));
        if let Some(hint) = hint {
            probe = probe.set_file_type(hint);
        }
        probe
            .guess_file_type()
            .map_err(|e| error_chain(&e))?
            .read()
            .map_err(|e| error_chain(&e))
    };
    read(ParsingMode::BestAttempt).or_else(|first| read(ParsingMode::Relaxed).map_err(|_| first))
}

/// An error and every cause under it, `outer: inner: …`.
///
/// lofty's `FileParseError` prints only "failed to parse Mpeg file"; the
/// reason — which frame, which tag — is its `source`, and a log that
/// stops at the outer message gives nothing to diagnose (#873).
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !out.ends_with(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        source = cause.source();
    }
    out
}

#[cfg(test)]
mod nested_chapter_tests;
#[cfg(test)]
mod tests;

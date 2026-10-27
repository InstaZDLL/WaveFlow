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
    extension_for_mime, extract_cover, extract_folder_cover, ArtworkSource, ExtractedCover,
    FOLDER_COVER_EXTENSIONS,
};
pub use probe::{probe_playable_stream, StreamInfo};

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
/// `.aifc` is deliberately not listed. symphonia's AIFC support covers
/// the PCM-shaped compression types (`none`, `sowt`, `twos`, `fl32`,
/// `alaw`, …) and refuses the rest, so accepting the extension would
/// index files that cannot play, on a format nobody writes any more.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "aiff", "aif", "ogg", "oga", "opus", "m4a", "mp4", "aac",
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
/// path. 1 MiB each — large enough that distinct tracks differ inside
/// the window (leading frames) and that tag rewrites land in it (ID3v2
/// at the head, ID3v1 / APE / Lyrics3 at the tail), small enough that a
/// multi-MB track reads ~2 MiB instead of its whole length.
const HASH_CHUNK_BYTES: u64 = 1024 * 1024;

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
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
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
    pub album: Option<String>,
    /// Raw Album Artist text from the source tag (`TPE2` / `aART` /
    /// `ALBUMARTIST` / `Album Artist`). Used as the album-grouping
    /// authority — when present, two tracks share an album even if
    /// their per-track Artist tags differ (featurings, lead-vocal
    /// rotations on K-pop EPs, etc.).
    pub album_artist: Option<String>,
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
    /// Embedded cover art extracted and hash-addressed during the scan. Only
    /// the first picture is kept (lofty exposes them in order and the first
    /// is usually the `CoverFront`). `None` when the tag has no pictures.
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
/// answered `None`, and `"MusicBee|4|0"` is not a number.
///
/// The star form is rescaled on the same 51-per-star scale the app
/// writes, so four stars stay four stars. It is lossy in one direction
/// and unavoidably so: the byte another tagger wrote is mapped to a
/// star by lofty before we ever see it, and half-stars have no
/// representation there at all. A rating this app wrote is not affected
/// — the file keeps the byte, and the database keeps it too.
pub fn extract_rating(tag: &Tag) -> Option<u8> {
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
        if let Ok(val) = trimmed.parse::<u16>() {
            let clamped = val.min(100);
            // Rounded to the nearest byte, not truncated. The app writes
            // this scale as `byte * 100 / 255`, so the two halves have
            // to meet: truncating sent 128 back as 127, and the scan
            // that follows a tag write overwrites `track.rating` with
            // what it read. Every value the star widget produces —
            // whole stars and halves, 26 through 255 — now survives the
            // round trip exactly.
            return Some(((clamped * 255 + 50) / 100) as u8);
        }
    }
    None
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

    let read = |mode: ParsingMode| -> Result<lofty::file::TaggedFile, String> {
        Probe::open(path)
            .map_err(|e| error_chain(&e))?
            .options(ParseOptions::new().parsing_mode(mode))
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
mod tests;

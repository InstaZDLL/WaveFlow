//! Podcasts, as far as they can be understood without a socket or a database
//! ([RFC-009](../../../docs/rfcs/RFC-009-podcasts.md)).
//!
//! Every byte read here came from a third party, so everything here reads a
//! hostile document:
//!
//! - [`feed`]: RSS 2.0, Atom and RSS 1.0, with a DTD refused, a node ceiling,
//!   text bounded before and after it is parsed, and at most
//!   [`MAX_EPISODES_PER_FEED`] episodes.
//! - [`opml`]: a subscription list, read under the same rules and written.
//! - [`chapters`]: Podcasting 2.0 JSON and inline `psc:chapters`, and the rule
//!   that picks one source.
//! - [`transcript`]: which of an episode's transcripts to open, and what it
//!   becomes: a [`lyrics::Document`](crate::lyrics::Document) when it is timed.
//! - [`address`]: what a feed may name, and the words a failure is stored as.
//!
//! Nothing here logs. A feed's address may hold a private token in its path
//! as often as in its query, so the rule in the app is that it reaches no log
//! line at all, and a parser that never logs cannot break it.

pub mod address;
pub mod chapters;
pub mod feed;
pub mod opml;
pub mod transcript;

pub use address::{FetchFailure, MAX_REDIRECTS};
pub use chapters::Chapter;
pub use feed::{Feed, FeedEpisode, FeedError, MAX_EPISODES_PER_FEED};
pub use opml::OpmlEntry;
pub use transcript::{TranscriptKind, TranscriptRef};

/// The largest feed read, in bytes. Also the fetcher's bound while it reads. A daily
/// show's feed really is this kind of size: one keeping every episode since 2017 weighs
/// 20 MiB.
pub const MAX_FEED_BYTES: usize = 32 * 1024 * 1024;

/// The largest chapter file read, in bytes.
pub const MAX_CHAPTERS_BYTES: usize = 1024 * 1024;

/// The largest transcript read, in bytes.
pub const MAX_TRANSCRIPT_BYTES: usize = 5 * 1024 * 1024;

/// The longest time believed, in a duration or a chapter: 100 hours.
pub(crate) const MAX_PLAUSIBLE_MS: u64 = 100 * 3_600_000;

/// The largest side of a channel picture accepted, in pixels. Far above any
/// real cover, and what keeps a small file that declares an enormous image from
/// reaching the thumbnail job.
const MAX_COVER_SIDE: u32 = 8_192;

/// Whether bytes a feed pointed at may join the artwork cache: an image in a
/// format covers come in, of a sane size. Reads the header only, so a
/// decompression bomb is refused without being decoded.
pub fn accepts_cover(bytes: &[u8]) -> bool {
    use image::ImageFormat;
    let Ok(reader) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()
    else {
        return false;
    };
    if !matches!(
        reader.format(),
        Some(ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP | ImageFormat::Gif)
    ) {
        return false;
    }
    reader
        .into_dimensions()
        .is_ok_and(|(width, height)| width <= MAX_COVER_SIDE && height <= MAX_COVER_SIDE)
}

/// The audio formats an episode may be stored as, recognised from the bytes:
/// `(suffix, content type)`. Anything else is refused: a feed that serves a
/// page of HTML, or a binary, as its enclosure gets nothing written to disk.
pub fn sniff_audio(head: &[u8]) -> Option<(&'static str, &'static str)> {
    if head.starts_with(b"ID3")
        || (head.len() >= 2 && head[0] == 0xff && (head[1] & 0xe0) == 0xe0 && (head[1] & 0x06) != 0)
    {
        Some(("mp3", "audio/mpeg"))
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" {
        Some(("m4a", "audio/mp4"))
    } else if head.starts_with(b"OggS") {
        Some(("ogg", "audio/ogg"))
    } else if head.starts_with(b"fLaC") {
        Some(("flac", "audio/flac"))
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        Some(("wav", "audio/wav"))
    } else if head.len() >= 2 && head[0] == 0xff && (head[1] & 0xf6) == 0xf0 {
        // ADTS: twelve sync bits, layer zero.
        Some(("aac", "audio/aac"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests;

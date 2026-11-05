//! What a chained stream says it is playing.
//!
//! An Ogg station changes song by starting a new logical stream, and the
//! comment header of that stream names the song. The reader keeps each
//! header as a revision of its metadata log, so the revision to read is the
//! newest one the moment the reader reports the change: the audio before it
//! has been decoded, the audio after it has not. Behind a time-shift buffer
//! the reader only gets there when the listener does, so the title stays
//! with the audio without any bookkeeping.
//!
//! The log is drained on the way: nothing else reads it, and on a station
//! left playing it would grow by a revision per song, pictures included.

use symphonia::core::formats::FormatReader;
use symphonia::core::meta::{MetadataRevision, StandardTag};

/// The most one value may weigh once read. A comment header has no limit a
/// server is held to, and what comes out of it ends up on the player bar.
const MAX_TAG_BYTES: usize = 512;

/// The song a stream names. Either field is `None` when the stream does not
/// say, or says nothing usable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamTags {
    pub title: Option<String>,
    /// Every `ARTIST` of the header, joined by `"; "`.
    pub artist: Option<String>,
}

/// Where the tags of a stream go, each time a new one starts.
pub type TagSink = Box<dyn FnMut(StreamTags) + Send>;

/// One value as it is shown: within [`MAX_TAG_BYTES`], on one line, and
/// `None` unless a letter or a digit is left. Stations do send an empty
/// `TITLE`, or a lone `" -"` when their playout has neither half of
/// `Artist - Title`.
fn clean(raw: &str) -> Option<String> {
    let mut end = raw.len().min(MAX_TAG_BYTES);
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    let line: String = raw[..end]
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = line.trim();
    line.chars()
        .any(char::is_alphanumeric)
        .then(|| line.to_string())
}

impl StreamTags {
    fn from_revision(revision: &MetadataRevision) -> Self {
        let mut tags = Self::default();
        for tag in &revision.media.tags {
            match &tag.std {
                Some(StandardTag::TrackTitle(value)) if tags.title.is_none() => {
                    tags.title = clean(value);
                }
                Some(StandardTag::Artist(value)) => {
                    let Some(artist) = clean(value) else { continue };
                    tags.artist = Some(match tags.artist.take() {
                        Some(artists) if artists.len() < MAX_TAG_BYTES => {
                            format!("{artists}; {artist}")
                        }
                        Some(artists) => artists,
                        None => artist,
                    });
                }
                _ => {}
            }
        }
        tags.artist = tags.artist.as_deref().and_then(clean);
        tags
    }
}

/// A stream whose tags are being followed.
pub(super) struct TagWatch {
    sink: TagSink,
    /// The first stream's tags have gone out. Held until the first packet
    /// rather than sent at the open: a load that overtook this one is served
    /// first, and its station must not be named after that.
    pub(super) announced: bool,
    /// Whether the revision at the front of the log has been read. The log
    /// never gives up its last revision, so that is how a stream that brought
    /// none is told from one that brought the first.
    front_read: bool,
}

impl TagWatch {
    pub(super) fn new(sink: TagSink) -> Self {
        Self {
            sink,
            announced: false,
            front_read: false,
        }
    }

    /// A stream has started: hand its tags on, empty if it brought none, so
    /// the last song's title does not stay on the next one.
    pub(super) fn stream_started(&mut self, format: &mut dyn FormatReader) {
        let mut log = format.metadata();
        let fresh = !log.is_latest() || (!self.front_read && log.current().is_some());
        let tags = if fresh {
            log.skip_to_latest()
                .map(StreamTags::from_revision)
                .unwrap_or_default()
        } else {
            StreamTags::default()
        };
        self.front_read = log.current().is_some();
        (self.sink)(tags);
    }
}

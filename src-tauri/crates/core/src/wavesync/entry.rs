//! What the queue holds: a track somebody put in it, described without
//! saying where it lives.

use serde::{Deserialize, Serialize};

use super::{EntryId, ParticipantId, Refusal, MAX_FILE_BYTES};

/// Longest title or album kept, in characters.
const MAX_TITLE_CHARS: usize = 512;
/// Longest single credit kept, in characters.
const MAX_ARTIST_CHARS: usize = 256;
/// Most credits kept for one entry.
const MAX_ARTISTS: usize = 16;
/// Longest codec name accepted, in bytes.
const MAX_CODEC_BYTES: usize = 16;
/// A day: longer is not a track anybody queues.
const MAX_DURATION_MS: u64 = 24 * 60 * 60 * 1000;
/// The highest PCM rate the decoder plays.
const MAX_SAMPLE_RATE: u32 = 768_000;
/// 7.1, the widest layout the output mixes.
const MAX_CHANNELS: u16 = 8;

/// Codec names an entry may not carry yet (RFC-010 Decision 6): a stream
/// read by range does not reach the DSD pipeline.
const UNSHAREABLE_CODECS: &[&str] = &["dsd", "dsf", "dff"];

/// One track in the queue.
///
/// Everything here is shown to every participant, so it is what the owner
/// chooses to say about the track: never a path, a folder, a file name or a
/// library id. The bytes are read through [`EntryId`] only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: EntryId,
    /// Who added it, and whose machine serves its bytes. Set by the host
    /// from the connection the entry arrived on, whatever the guest sent.
    pub owner: ParticipantId,
    pub title: String,
    /// The credits, in order, one name each (never a joined string).
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub duration_ms: u64,
    /// The container or codec, lowercase (`flac`, `mp3`, `opus`, …): what
    /// the listener's probe is told to expect.
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// The file's size in bytes, which every ranged read is checked against.
    pub size: u64,
    /// The whole-file BLAKE3, hex, when the owner computed it: how a listener
    /// that holds the same file plays its own copy (RFC-006's rule).
    pub full_hash: Option<String>,
    /// The track's ReplayGain, so that every participant normalises it the
    /// same way.
    pub gain: Option<Gain>,
}

/// ReplayGain values, as the owner read them from the file.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Gain {
    pub track_gain_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_gain_db: Option<f32>,
    pub album_peak: Option<f32>,
}

impl Entry {
    /// The entry as the host keeps it: its owner set to whoever sent it, its
    /// text cut to the caps and stripped of control characters, its numbers
    /// checked. A peer's entry is hostile input, and it is about to be shown
    /// on every other screen.
    pub(super) fn admitted(mut self, owner: ParticipantId) -> Result<Self, Refusal> {
        self.owner = owner;
        self.title = clean(&self.title, MAX_TITLE_CHARS);
        if self.title.is_empty() {
            return Err(Refusal::InvalidEntry);
        }
        self.artists = self
            .artists
            .iter()
            .map(|a| clean(a, MAX_ARTIST_CHARS))
            .filter(|a| !a.is_empty())
            .take(MAX_ARTISTS)
            .collect();
        self.album = self
            .album
            .map(|a| clean(&a, MAX_TITLE_CHARS))
            .filter(|a| !a.is_empty());

        if self.codec.is_empty()
            || self.codec.len() > MAX_CODEC_BYTES
            || !self
                .codec
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return Err(Refusal::InvalidEntry);
        }
        if UNSHAREABLE_CODECS.contains(&self.codec.as_str()) {
            return Err(Refusal::NotShareable);
        }
        if self.size == 0 {
            return Err(Refusal::InvalidEntry);
        }
        if self.size > MAX_FILE_BYTES {
            return Err(Refusal::TooLarge);
        }
        if self.duration_ms == 0
            || self.duration_ms > MAX_DURATION_MS
            || self.sample_rate == 0
            || self.sample_rate > MAX_SAMPLE_RATE
            || self.channels == 0
            || self.channels > MAX_CHANNELS
        {
            return Err(Refusal::InvalidEntry);
        }
        if let Some(hash) = &self.full_hash {
            if hash.len() != 64 || !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                return Err(Refusal::InvalidEntry);
            }
        }
        self.gain = self.gain.map(Gain::finite);
        Ok(self)
    }
}

impl Gain {
    /// The same values with anything not finite dropped: a NaN gain would
    /// turn a listener's whole track into NaN samples before the clamp.
    fn finite(self) -> Self {
        let keep = |v: Option<f32>| v.filter(|v| v.is_finite());
        Self {
            track_gain_db: keep(self.track_gain_db),
            track_peak: keep(self.track_peak),
            album_gain_db: keep(self.album_gain_db),
            album_peak: keep(self.album_peak),
        }
    }
}

/// `text` trimmed, without control characters, cut to `max` characters.
pub(super) fn clean(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(max)
        .collect::<String>()
        .trim_end()
        .to_string()
}

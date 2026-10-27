//! HTTP Live Streaming playlists, as far as a radio needs them.
//!
//! A station that streams over HLS hands out an `.m3u8` that is either a
//! **multivariant** playlist (one line per quality, each naming a media
//! playlist) or a **media** playlist (the segments themselves, a sliding
//! window that the server extends every few seconds). Parsed here without
//! a network, so the rules can be tested on their own; fetching, resolving
//! the URIs against the playlist's own URL and polling belong to the caller.
//!
//! What the engine plays is a continuous audio stream: the caller demuxes
//! each segment ([`super::ts`]) and appends the audio to one buffer. So the
//! parts of HLS a radio does not use are refused rather than half-played:
//! an encrypted segment (`#EXT-X-KEY` other than `NONE`) and fragmented MP4
//! (`#EXT-X-MAP`), which no station probed used.

/// One quality of a multivariant playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub uri: String,
    /// `BANDWIDTH`, bits per second.
    pub bandwidth: Option<u64>,
    /// `CODECS`, as written (`"mp4a.40.2"`, `"avc1.4d401f,mp4a.40.2"`).
    pub codecs: Option<String>,
}

impl Variant {
    /// Whether the variant carries no video: its codecs, when it names
    /// them, are all audio ones.
    pub fn is_audio_only(&self) -> bool {
        match &self.codecs {
            Some(codecs) => codecs.split(',').all(|codec| {
                let codec = codec.trim().to_ascii_lowercase();
                codec.starts_with("mp4a") || codec.starts_with("mp3") || codec == "ac-3"
            }),
            // Unnamed codecs on a radio's playlist are audio more often
            // than not; a video variant would still play its sound.
            None => true,
        }
    }
}

/// A multivariant playlist: its qualities, and the audio renditions it
/// declares apart (`#EXT-X-MEDIA:TYPE=AUDIO`), which a video-first stream
/// uses to carry its sound on its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Multivariant {
    pub variants: Vec<Variant>,
    pub audio_renditions: Vec<String>,
}

impl Multivariant {
    /// The media playlist to play: an audio rendition when there is one,
    /// else the audio-only variant with the most bandwidth, else — a stream
    /// that only offers video variants — the one with the least, whose
    /// sound is the same and whose video costs least to download.
    pub fn pick(&self) -> Option<&str> {
        if let Some(uri) = self.audio_renditions.first() {
            return Some(uri);
        }
        let audio = self
            .variants
            .iter()
            .filter(|v| v.is_audio_only())
            .max_by_key(|v| v.bandwidth.unwrap_or(0));
        audio
            .or_else(|| {
                self.variants
                    .iter()
                    .min_by_key(|v| v.bandwidth.unwrap_or(u64::MAX))
            })
            .map(|v| v.uri.as_str())
    }
}

/// One segment of a media playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub uri: String,
    /// `#EXTINF`, in seconds.
    pub duration_s: f64,
    /// Its media sequence number: the first segment's is the playlist's
    /// `#EXT-X-MEDIA-SEQUENCE`, each next one one more.
    pub sequence: u64,
}

/// A media playlist: the window of segments the server offers now.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaPlaylist {
    /// `#EXT-X-TARGETDURATION`, seconds: no segment is longer, and the
    /// server is asked again about this often.
    pub target_duration_s: f64,
    pub segments: Vec<Segment>,
    /// `#EXT-X-ENDLIST`: the list is complete, nothing will be added.
    pub ended: bool,
}

/// A playlist, whichever kind it is.
#[derive(Debug, Clone, PartialEq)]
pub enum Playlist {
    Multivariant(Multivariant),
    Media(MediaPlaylist),
}

/// Why a playlist cannot be played.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HlsError {
    #[error("not an HLS playlist")]
    NotHls,
    #[error("the segments are encrypted")]
    Encrypted,
    #[error("the segments are fragmented MP4")]
    FragmentedMp4,
    #[error("the segments are byte ranges of one file")]
    ByteRange,
    #[error("the playlist names nothing to play")]
    Empty,
}

/// Whether `text` is an HLS playlist rather than a station list: an
/// `#EXTM3U` that uses `#EXT-X-` tags.
pub fn is_hls(text: &str) -> bool {
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with("#EXTM3U") && text.contains("#EXT-X-")
}

/// The value of `KEY=` in an attribute list, quoted or not.
fn attribute<'a>(list: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = list;
    while !rest.is_empty() {
        let (name, after) = rest.split_once('=')?;
        let (value, next) = if let Some(quoted) = after.strip_prefix('"') {
            let end = quoted.find('"')?;
            (&quoted[..end], quoted[end + 1..].trim_start_matches(','))
        } else {
            match after.split_once(',') {
                Some((value, next)) => (value, next),
                None => (after, ""),
            }
        };
        if name.trim().eq_ignore_ascii_case(key) {
            return Some(value);
        }
        rest = next;
    }
    None
}

/// A duration as a playlist writes it, when it is one a segment can have:
/// finite, above zero, at most an hour. `inf`, `NaN` and absurd figures
/// parse as floats, and would reach the lag arithmetic.
fn seconds(raw: &str) -> Option<f64> {
    raw.trim()
        .parse::<f64>()
        .ok()
        .filter(|s| s.is_finite() && *s > 0.0 && *s <= 3600.0)
}

pub fn parse(text: &str) -> Result<Playlist, HlsError> {
    if !is_hls(text) {
        return Err(HlsError::NotHls);
    }
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim_start_matches('\u{feff}').trim())
        .filter(|l| !l.is_empty())
        .collect();

    if lines.iter().any(|l| l.starts_with("#EXT-X-STREAM-INF")) {
        let mut out = Multivariant::default();
        let mut pending: Option<(Option<u64>, Option<String>)> = None;
        for line in &lines {
            if let Some(attrs) = line.strip_prefix("#EXT-X-STREAM-INF:") {
                pending = Some((
                    attribute(attrs, "BANDWIDTH").and_then(|b| b.parse().ok()),
                    attribute(attrs, "CODECS").map(str::to_string),
                ));
            } else if let Some(attrs) = line.strip_prefix("#EXT-X-MEDIA:") {
                if attribute(attrs, "TYPE").is_some_and(|t| t.eq_ignore_ascii_case("AUDIO")) {
                    if let Some(uri) = attribute(attrs, "URI") {
                        out.audio_renditions.push(uri.to_string());
                    }
                }
            } else if !line.starts_with('#') {
                if let Some((bandwidth, codecs)) = pending.take() {
                    out.variants.push(Variant {
                        uri: line.to_string(),
                        bandwidth,
                        codecs,
                    });
                }
            }
        }
        if out.variants.is_empty() && out.audio_renditions.is_empty() {
            return Err(HlsError::Empty);
        }
        return Ok(Playlist::Multivariant(out));
    }

    let mut out = MediaPlaylist {
        target_duration_s: 6.0,
        ..MediaPlaylist::default()
    };
    let mut sequence: u64 = 0;
    let mut duration: Option<f64> = None;
    for line in &lines {
        if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            if let Some(v) = seconds(value) {
                out.target_duration_s = v;
            }
        } else if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            sequence = value.trim().parse().unwrap_or(0);
        } else if let Some(attrs) = line.strip_prefix("#EXT-X-KEY:") {
            if !attribute(attrs, "METHOD").is_some_and(|m| m.eq_ignore_ascii_case("NONE")) {
                return Err(HlsError::Encrypted);
            }
        } else if line.starts_with("#EXT-X-MAP") {
            return Err(HlsError::FragmentedMp4);
        } else if line.starts_with("#EXT-X-BYTERANGE") {
            // Each segment a slice of one file: fetching the URI would
            // fetch the whole file once per slice.
            return Err(HlsError::ByteRange);
        } else if line.starts_with("#EXT-X-ENDLIST") {
            out.ended = true;
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            duration = value.split(',').next().and_then(seconds);
        } else if !line.starts_with('#') {
            out.segments.push(Segment {
                uri: line.to_string(),
                duration_s: duration.take().unwrap_or(out.target_duration_s),
                sequence,
            });
            sequence += 1;
        }
    }
    if out.segments.is_empty() && out.ended {
        return Err(HlsError::Empty);
    }
    Ok(Playlist::Media(out))
}

#[cfg(test)]
mod tests;

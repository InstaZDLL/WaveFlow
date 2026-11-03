//! The decoder's view of a file lofty cannot read: whether the player
//! could play it, and what it would play.

use std::fs;
use std::path::Path;

use lofty::file::FileType;

/// What the player's decoder can tell about a file whose tags no reader
/// accepts: enough to list it and play it.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub duration_ms: i64,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    pub bit_depth: Option<i64>,
    /// The decoder's codec, as the format chip shows it (`MP3`, `PCM`):
    /// what the stream is, not what the file is called.
    pub codec: Option<String>,
}

/// The channel count the player's decoder sees in a file lofty did read, or
/// why it cannot decode it. `Ok(None)` for a container whose type already
/// implies a codec the player has.
///
/// lofty accepting an MP4 or an ADTS file says nothing about whether it
/// plays: an xHE-AAC `.m4a` has perfectly good tags. `file_type` is what
/// lofty found in the file, so a misnamed one is checked as well. This opens
/// the stream the way playback does and decodes until a packet comes out,
/// since what a stream declares is not always what it plays: a layout can
/// travel in-band. That count wins over lofty's, which is wrong on this
/// family: measured 7 for 7.1 AAC, 2 for every stream with a PCE, 64 for
/// AAC-ELD.
pub fn decoder_channels(path: &Path, file_type: FileType) -> Result<Option<i64>, String> {
    if !decoder_decides(file_type) {
        return Ok(None);
    }
    channels_of(open_stream(path)?)
}

/// Whether [`decoder_channels`] has anything to say about this family.
pub fn decoder_decides(file_type: FileType) -> bool {
    matches!(file_type, FileType::Mp4 | FileType::Aac)
}

/// [`decoder_channels`] over any source, for a file that is not on this disk.
/// `ext` is the extension it carries, as a hint for the probe.
pub fn decoder_channels_from(
    source: Box<dyn symphonia::core::io::MediaSource>,
    ext: Option<&str>,
) -> Result<Option<i64>, String> {
    channels_of(open_source(source, ext)?)
}

fn channels_of(opened: OpenedStream) -> Result<Option<i64>, String> {
    let OpenedStream {
        mut format,
        track_id,
        mut decoder,
        ..
    } = opened;
    let mut tried = 0usize;
    while tried < PLAYABLE_PROBE_PACKETS {
        let Ok(Some(packet)) = format.next_packet() else {
            break;
        };
        if packet.track_id != track_id {
            continue;
        }
        tried += 1;
        if let Ok(decoded) = decoder.decode(&packet) {
            return Ok(Some(decoded.spec().channels().count() as i64));
        }
    }
    Err(format!(
        "the player cannot decode this stream: none of its first {tried} packets decodes"
    ))
}

/// A stream opened the way playback opens it: the app's probe, which
/// judges by content, and the one codec registry
/// ([`crate::audio_format::opus::codecs`]).
struct OpenedStream {
    format: Box<dyn symphonia::core::formats::FormatReader>,
    track_id: u32,
    time_base: Option<symphonia::core::units::TimeBase>,
    track_duration: Option<symphonia::core::units::Duration>,
    num_frames: Option<u64>,
    params: symphonia::core::codecs::audio::AudioCodecParameters,
    decoder: Box<dyn symphonia::core::codecs::audio::AudioDecoder>,
}

fn open_stream(path: &Path) -> Result<OpenedStream, String> {
    let file = fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    open_source(Box::new(file), path.extension().and_then(|e| e.to_str()))
}

fn open_source(
    source: Box<dyn symphonia::core::io::MediaSource>,
    ext: Option<&str>,
) -> Result<OpenedStream, String> {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let mut hint = Hint::new();
    if let Some(ext) = ext {
        hint.with_extension(ext);
    }
    let format = crate::audio_format::probe()
        .probe(
            &hint,
            MediaSourceStream::new(source, Default::default()),
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("the player cannot read this container: {e}"))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| "the file holds no audio track".to_string())?;
    let (track_id, time_base, track_duration, num_frames) =
        (track.id, track.time_base, track.duration, track.num_frames);
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| "the audio track has no codec parameters".to_string())?
        .clone();
    let decoder = crate::audio_format::opus::codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| format!("the player cannot decode this stream: {e}"))?;
    Ok(OpenedStream {
        format,
        track_id,
        time_base,
        track_duration,
        num_frames,
        params,
        decoder,
    })
}

/// How many packets may fail to decode before the stream is judged
/// unplayable. A few leading packets can be garbage left by a broken
/// tag; a stream with none decodable in this many is not music.
const PLAYABLE_PROBE_PACKETS: usize = 64;

/// Whether the player could play this file, and what it would play, for
/// a file lofty cannot read at all (#873).
///
/// Opened the way playback opens it — the app's probe, which judges
/// by content, and the one codec registry
/// ([`crate::audio_format::opus::codecs`]) — and accepted only once a
/// packet actually decodes, so a file listed this way is one the player
/// will play: the scanner and the decoder cannot disagree. `None` when
/// any of that fails.
pub fn probe_playable_stream(path: &Path) -> Option<StreamInfo> {
    let file = fs::File::open(path).ok()?;
    probe_playable_source(Box::new(file), path.extension().and_then(|e| e.to_str()))
}

/// [`probe_playable_stream`] over any source, for a file that is not on this
/// disk. `ext` is the extension it carries, as a hint for the probe.
pub fn probe_playable_source(
    source: Box<dyn symphonia::core::io::MediaSource>,
    ext: Option<&str>,
) -> Option<StreamInfo> {
    let OpenedStream {
        mut format,
        track_id,
        time_base,
        track_duration,
        num_frames,
        params,
        mut decoder,
    } = open_source(source, ext).ok()?;
    let codec = crate::audio_format::opus::codecs()
        .get_audio_decoder(params.codec)
        .map(|registered| crate::audio_format::codec_label(registered.codec.info.short_name));

    let mut summed = 0u64;
    let mut decoded = false;
    let mut tried = 0usize;
    while let Ok(Some(packet)) = format.next_packet() {
        if packet.track_id != track_id {
            continue;
        }
        summed = summed.saturating_add(packet.dur.get());
        if !decoded {
            tried += 1;
            decoded = decoder.decode(&packet).is_ok();
            if !decoded && tried >= PLAYABLE_PROBE_PACKETS {
                return None;
            }
            // The container's own duration, when it has one, saves
            // walking the rest of the file.
            if decoded && (track_duration.is_some() || num_frames.is_some()) {
                break;
            }
        }
    }
    if !decoded {
        return None;
    }

    let sample_rate = params.sample_rate;
    let to_ms = |d: symphonia::core::units::Duration| {
        time_base
            .and_then(|tb| tb.calc_duration(d))
            .map(|t| (t.as_micros() / 1000) as i64)
    };
    let duration_ms = track_duration
        .and_then(to_ms)
        .or_else(|| {
            num_frames
                .zip(sample_rate.filter(|r| *r > 0))
                .map(|(n, r)| (n.saturating_mul(1000) / u64::from(r)) as i64)
        })
        .or_else(|| to_ms(symphonia::core::units::Duration::from(summed)))
        .unwrap_or(0);

    Some(StreamInfo {
        duration_ms,
        sample_rate: sample_rate.map(i64::from),
        // The decoder's, which may know better than the container: an MP4 with
        // a program config element declares no count at all.
        channels: decoder
            .codec_params()
            .channels
            .as_ref()
            .map(|c| c.count() as i64),
        bit_depth: params.bits_per_sample.map(i64::from).filter(|b| *b > 0),
        codec,
    })
}

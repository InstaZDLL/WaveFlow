//! The decoder's view of a file lofty cannot read: whether the player
//! could play it, and what it would play.

use std::fs;
use std::path::Path;

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

/// How many packets may fail to decode before the stream is judged
/// unplayable. A few leading packets can be garbage left by a broken
/// tag; a stream with none decodable in this many is not music.
const PLAYABLE_PROBE_PACKETS: usize = 64;

/// Whether the player could play this file, and what it would play, for
/// a file lofty cannot read at all (#873).
///
/// Opened the way playback opens it — symphonia's probe, which judges
/// by content, and the one codec registry
/// ([`crate::audio_format::opus::codecs`]) — and accepted only once a
/// packet actually decodes, so a file listed this way is one the player
/// will play: the scanner and the decoder cannot disagree. `None` when
/// any of that fails.
pub fn probe_playable_stream(path: &Path) -> Option<StreamInfo> {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let file = fs::File::open(path).ok()?;
    let mut hint = Hint::new();
    let ext = path.extension().and_then(|e| e.to_str());
    if let Some(ext) = ext {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            MediaSourceStream::new(Box::new(file), Default::default()),
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .ok()?;
    let track = format.default_track(TrackType::Audio)?;
    let track_id = track.id;
    let time_base = track.time_base;
    let track_duration = track.duration;
    let num_frames = track.num_frames;
    let params = track.codec_params.as_ref()?.audio()?.clone();
    let registry = crate::audio_format::opus::codecs();
    let mut decoder = registry
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .ok()?;
    let codec = registry
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
        channels: params.channels.as_ref().map(|c| c.count() as i64),
        bit_depth: params.bits_per_sample.map(i64::from).filter(|b| *b > 0),
        codec,
    })
}

//! Opus decoding, as a symphonia [`AudioDecoder`] over libopus (#581).
//!
//! symphonia reads Opus containers perfectly well — `symphonia-format-ogg`
//! ships a complete Opus mapper that parses `OpusHead`, works out the
//! per-packet delay trim and turns `OpusTags` into Vorbis comments, so
//! tags and durations were already correct. The only missing piece was
//! the decoder itself: `symphonia-codec-opus` does not exist.
//!
//! ## Why a C binding
//!
//! Two pure-Rust decoders were measured rather than assessed from
//! their READMEs, and both were rejected for opposite reasons.
//! `libopus-rs` covers CELT only and refuses honestly outside that
//! range — too narrow to ship, but it fails loudly. `opus-rs` decodes
//! everything and **never returns an error**, including on the files
//! it gets wrong: a 48 kb/s stereo file that is 94 % CELT and 6 %
//! hybrid scores 30 dB against a libopus reference. Silent audio
//! corruption with nothing for the player to catch.
//!
//! Writing our own is not on the table — Opus is a range coder plus
//! CELT plus SILK plus hybrid plus mode switching, and every bug in it
//! is a corrupted playback.
//!
//! So: `opusic-sys`, which vendors libopus 1.6.1 and builds it
//! statically. Static on every platform on purpose — the Linux
//! packages are repackaged from the release binary rather than built
//! from source, so a system libopus would have to be added as a
//! runtime dependency to three packaging manifests and bundled into
//! the AppImage, to gain nothing.
//!
//! ## Pre-skip
//!
//! Every Opus stream starts with a few hundred frames of encoder
//! priming that must never be played, and this decoder counts them
//! itself.
//!
//! That is not where it looked like it belonged. The Ogg reader does
//! turn `OpusHead`'s pre-skip into `Track::delay`, and it hands every
//! decoder a per-packet `trim_start` — which is exactly how the Vorbis
//! decoder next door disposes of its own priming. But the Opus packet
//! parser in `symphonia-format-ogg` 0.6.1 returns a discard of **zero**
//! for every packet, so that trim is always empty. Measured, not read:
//! a 3-second file decoded to 144 312 frames instead of 144 000, the
//! difference being precisely the 312-frame pre-skip.
//!
//! `packet.trim_start` is still honoured and counted against what is
//! owed, so if the mapper ever starts reporting it the priming is not
//! dropped twice.
//!
//! ## Output gain
//!
//! `OpusHead` carries an output gain that RFC 7845 says a player
//! SHOULD always apply — it is part of the file's intended level, not
//! a ReplayGain-style suggestion the listener may switch off. It is
//! handed to libopus, which applies it inside the decoder.

use std::ffi::{c_int, CStr};
use std::sync::OnceLock;

use symphonia::core::audio::{
    layouts::{CHANNEL_LAYOUT_MONO, CHANNEL_LAYOUT_STEREO},
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, Channels, GenericAudioBufferRef,
};
use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia::core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia::core::codecs::registry::{
    CodecRegistry, RegisterableAudioDecoder, SupportedAudioCodec,
};
use symphonia::core::codecs::CodecInfo;
use symphonia::core::errors::{decode_error, unsupported_error, Result};
use symphonia::core::packet::PacketRef;

/// The codec registry this app decodes with: everything symphonia's
/// feature flags enable, plus Opus.
///
/// Built once and shared, the way `symphonia::default::get_codecs`
/// is. There are three places that open an audio stream — playback,
/// the analysis pass, and the scanner's probe — and each used to reach
/// for the default registry directly. Going through one function is
/// what keeps them from disagreeing about which formats the app can
/// play, which is the disagreement that puts an unplayable track in
/// the library.
pub fn codecs() -> &'static CodecRegistry {
    static REGISTRY: OnceLock<CodecRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut registry);
        registry.register_audio_decoder::<OpusDecoder>();
        registry
    })
}

/// Opus always decodes at 48 kHz regardless of what the source was
/// encoded from; `OpusHead`'s "input sample rate" is documentation of
/// the original, not an instruction.
const OPUS_RATE: u32 = 48_000;

/// Longest frame Opus can carry, in samples per channel at 48 kHz:
/// 120 ms. The decode buffer is sized for it once rather than grown
/// per packet.
const MAX_FRAME_SIZE: usize = 5_760;

/// What `OpusHead` says, minus the parts a decoder does not need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpusHead {
    channel_count: u8,
    /// Encoder priming, in 48 kHz samples, that must never be played.
    pre_skip: u16,
    /// Q7.8 fixed-point dB, applied by libopus.
    output_gain: i16,
    mapping_family: u8,
}

/// The channel layout a `mapping_family` implies, in the form
/// `opus_multistream_decoder_create` wants.
struct ChannelMapping {
    streams: c_int,
    coupled_streams: c_int,
    /// One entry per output channel, naming the decoded channel it
    /// comes from.
    mapping: Vec<u8>,
}

/// Parse the parts of `OpusHead` the decoder needs, plus the channel
/// mapping table that follows it for families other than 0.
///
/// Deliberately strict about the magic and the version: a truncated or
/// foreign header reaching libopus is a crash surface, and refusing
/// here costs one comparison.
fn parse_head(extra: &[u8]) -> Result<(OpusHead, ChannelMapping)> {
    if extra.len() < 19 || &extra[..8] != b"OpusHead" {
        return decode_error("opus: extra data is not an OpusHead");
    }
    // The version's major half must be 0; RFC 7845 says a decoder may
    // ignore the minor half, which is what masking does.
    if extra[8] & 0xF0 != 0 {
        return unsupported_error("opus: unsupported OpusHead version");
    }
    let head = OpusHead {
        channel_count: extra[9],
        pre_skip: u16::from_le_bytes([extra[10], extra[11]]),
        output_gain: i16::from_le_bytes([extra[16], extra[17]]),
        mapping_family: extra[18],
    };
    if head.channel_count == 0 {
        return decode_error("opus: OpusHead declares no channels");
    }

    let mapping = match head.mapping_family {
        // Family 0 has no mapping table: one stream, coupled when it
        // is stereo. This is what essentially every music file is.
        0 => {
            if head.channel_count > 2 {
                return decode_error("opus: mapping family 0 allows at most two channels");
            }
            ChannelMapping {
                streams: 1,
                coupled_streams: c_int::from(head.channel_count == 2),
                mapping: (0..head.channel_count).collect(),
            }
        }
        // Families 1 (Vorbis surround order) and 255 (discrete) both
        // carry an explicit table. Ambisonics (2, 3) are refused
        // rather than guessed at.
        1 | 255 => {
            // stream count, coupled count, then one byte per channel.
            let table = 19 + 2 + head.channel_count as usize;
            if extra.len() < table {
                return decode_error("opus: OpusHead channel mapping table is truncated");
            }
            let streams = extra[19];
            let coupled = extra[20];
            if streams == 0 || coupled > streams {
                return decode_error("opus: OpusHead declares an impossible stream layout");
            }
            ChannelMapping {
                streams: c_int::from(streams),
                coupled_streams: c_int::from(coupled),
                mapping: extra[21..table].to_vec(),
            }
        }
        family => {
            return unsupported_error(match family {
                2 | 3 => "opus: ambisonic channel mapping is not supported",
                _ => "opus: unknown channel mapping family",
            })
        }
    };
    Ok((head, mapping))
}

/// Turn a libopus error code into the message libopus itself would
/// print, so a failure says what went wrong rather than a number.
fn opus_message(code: c_int) -> &'static str {
    // SAFETY: `opus_strerror` returns a pointer to a static string for
    // every input, including codes it does not recognise.
    let text = unsafe { CStr::from_ptr(opusic_sys::opus_strerror(code)) };
    text.to_str().unwrap_or("unknown opus error")
}

/// A libopus multistream decoder and the buffer it decodes into.
///
/// Multistream even for ordinary stereo: family 0 is the one-stream
/// case of the same API, so there is a single code path rather than
/// two that could drift.
pub struct OpusDecoder {
    /// Owned. Freed in [`Drop`], and never null after construction.
    state: *mut opusic_sys::OpusMSDecoder,
    params: AudioCodecParameters,
    opts: AudioDecoderOptions,
    channels: usize,
    /// Frames of encoder priming still to drop before any of the
    /// output is real audio.
    ///
    /// Counted here because nothing else does. The Ogg reader turns
    /// `OpusHead`'s pre-skip into `Track::delay`, but its Opus packet
    /// parser reports a per-packet discard of **zero** for every
    /// packet — measured on `symphonia-format-ogg` 0.6.1, and unlike
    /// the Vorbis parser next to it, which is why the Vorbis decoder
    /// can simply honour `packet.trim_start`. Left to itself the
    /// stream plays 312 frames of priming at the head of every track.
    ///
    /// One consequence worth knowing: resuming a track at a saved
    /// position seeks without resetting the decoder (the PCM path
    /// does, unlike the DoP one), so this counter is still owed and
    /// the first 312 frames after the resume are trimmed instead. That
    /// is 6.5 ms, in the direction RFC 7845 §4.2 asks for at a seek
    /// anyway — not worth changing a seek path every codec shares.
    pending_discard: u32,
    /// libopus writes interleaved; symphonia wants planes. One scratch
    /// buffer, allocated once.
    interleaved: Vec<f32>,
    buf: AudioBuffer<f32>,
}

// SAFETY: an `OpusMSDecoder` is a plain state struct with no interior
// threading and no globals; libopus documents one decoder per stream
// used from one thread at a time. `OpusDecoder` owns its pointer
// exclusively and hands out no copies, so moving it between threads —
// which is what symphonia's `AudioDecoder: Send + Sync` bound needs,
// the decoder living on the app's decode thread — is sound. `&self`
// methods never touch the pointer.
unsafe impl Send for OpusDecoder {}
unsafe impl Sync for OpusDecoder {}

impl OpusDecoder {
    fn try_new(params: &AudioCodecParameters, opts: &AudioDecoderOptions) -> Result<Self> {
        let Some(extra) = params.extra_data.as_deref() else {
            return decode_error("opus: missing OpusHead");
        };
        let (head, mapping) = parse_head(extra)?;
        let channels = usize::from(head.channel_count);

        let mut error: c_int = 0;
        // SAFETY: `mapping.mapping` holds exactly `channel_count`
        // entries (checked in `parse_head`), which is the length
        // libopus reads for the declared channel count. `error` is a
        // live local.
        let state = unsafe {
            opusic_sys::opus_multistream_decoder_create(
                OPUS_RATE as i32,
                channels as c_int,
                mapping.streams,
                mapping.coupled_streams,
                mapping.mapping.as_ptr(),
                &mut error,
            )
        };
        if state.is_null() || error != opusic_sys::OPUS_OK {
            if !state.is_null() {
                // libopus documents a null return on failure, so this
                // pairing should not occur — freeing it anyway keeps
                // ownership a local rule rather than one inherited from
                // the library's contract.
                // SAFETY: non-null, owned, and not yet handed anywhere.
                unsafe { opusic_sys::opus_multistream_decoder_destroy(state) };
            }
            return decode_error(opus_message(error));
        }

        let decoder = Self {
            state,
            params: params.clone(),
            opts: *opts,
            channels,
            pending_discard: u32::from(head.pre_skip),
            interleaved: vec![0.0; MAX_FRAME_SIZE * channels],
            buf: AudioBuffer::new(
                AudioSpec::new(OPUS_RATE, channel_layout(head.channel_count)),
                MAX_FRAME_SIZE,
            ),
        };

        if head.output_gain != 0 {
            // SAFETY: `state` is non-null and owned; OPUS_SET_GAIN
            // takes one `opus_int32` through the variadic tail, and
            // any value is in range — libopus clamps it itself.
            let status = unsafe {
                opusic_sys::opus_multistream_decoder_ctl(
                    decoder.state,
                    opusic_sys::OPUS_SET_GAIN_REQUEST,
                    i32::from(head.output_gain),
                )
            };
            if status != opusic_sys::OPUS_OK {
                // Not fatal: the file plays, at the wrong level. The
                // alternative is refusing a track over a gain field.
                tracing::warn!(
                    gain = head.output_gain,
                    reason = opus_message(status),
                    "opus: header output gain refused, playing at unity"
                );
            }
        }

        Ok(decoder)
    }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        self.buf.clear();

        // An empty packet is not silence, it is the absence of a
        // packet — and libopus reads that as *packet loss* and
        // synthesises concealment audio rather than failing. Measured,
        // not assumed. Letting it through would invent audio the file
        // does not contain; a reader that hands us nothing has a
        // problem worth reporting.
        if packet.data.is_empty() {
            return decode_error("opus: empty packet");
        }

        // SAFETY: `interleaved` holds `MAX_FRAME_SIZE * channels`
        // samples, and `frame_size` caps what libopus may write at
        // `MAX_FRAME_SIZE` per channel. `packet.data` is a live slice;
        // its length is passed alongside it.
        let decoded = unsafe {
            opusic_sys::opus_multistream_decode_float(
                self.state,
                packet.data.as_ptr(),
                packet.data.len() as i32,
                self.interleaved.as_mut_ptr(),
                MAX_FRAME_SIZE as c_int,
                0,
            )
        };
        if decoded < 0 {
            return decode_error(opus_message(decoded));
        }
        let frames = decoded as usize;

        self.buf.render_uninit(Some(frames));
        for channel in 0..self.channels {
            let Some(plane) = self.buf.plane_mut(channel) else {
                return decode_error("opus: missing audio plane");
            };
            for (frame, sample) in plane.iter_mut().enumerate().take(frames) {
                *sample = self.interleaved[frame * self.channels + channel];
            }
        }

        if self.opts.gapless {
            // Whatever the reader already trimmed counts toward the
            // priming, so a mapper that one day starts reporting a
            // discard for Opus does not make us drop it twice.
            // Bounded by what was actually decoded: a reader that
            // reports more priming than the packet produced must not
            // be able to ask for a trim past the end of the buffer.
            let reported = (packet.trim_start.get() as usize).min(frames);
            let still_owed = (self.pending_discard as usize).saturating_sub(reported);
            let extra = still_owed.min(frames.saturating_sub(reported));
            self.pending_discard = self
                .pending_discard
                .saturating_sub((reported + extra) as u32);
            self.buf
                .trim(reported + extra, packet.trim_end.get() as usize);
        }
        Ok(())
    }
}

impl Drop for OpusDecoder {
    fn drop(&mut self) {
        // SAFETY: `state` was returned by `opus_multistream_decoder_create`,
        // is owned exclusively, and is destroyed exactly once.
        unsafe { opusic_sys::opus_multistream_decoder_destroy(self.state) };
    }
}

impl AudioDecoder for OpusDecoder {
    fn reset(&mut self) {
        // After a seek the decoder holds overlap state from a part of
        // the stream that no longer precedes the next packet. libopus
        // has no public "reset" on a multistream decoder, so the state
        // is rebuilt — cheap next to the seek that caused it, and the
        // honest alternative to playing one frame of the wrong audio.
        //
        // A failure here leaves the previous decoder in place rather
        // than poisoning the stream: the worst outcome is the stale
        // overlap this is trying to avoid, which is a glitch and not a
        // silence.
        if let Ok(mut fresh) = Self::try_new(&self.params, &self.opts) {
            // The priming belongs to the start of the stream. A reset
            // means a seek landed us in the middle of one, where there
            // is nothing to drop — re-applying the pre-skip here would
            // silently eat 312 frames of real audio on every seek.
            //
            // Not dropped either: RFC 7845 §4.2 suggests decoding ~80 ms
            // before the seek point and discarding it, so the decoder
            // has converged. We do not, deliberately — that audio is
            // audio the listener asked for, and the convergence
            // artefact is brief and bounded where the loss would not be.
            fresh.pending_discard = 0;
            *self = fresh;
        }
    }

    fn codec_info(&self) -> &CodecInfo {
        &Self::supported_codecs()
            .first()
            .expect("at least one codec registered")
            .info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        match self.decode_inner(packet) {
            Ok(()) => Ok(self.buf.as_generic_audio_buffer_ref()),
            Err(err) => {
                // The trait requires an empty buffer after a failure,
                // so a caller that ignores the error cannot replay the
                // previous packet's audio.
                self.buf.clear();
                Err(err)
            }
        }
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for OpusDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>> {
        Ok(Box::new(OpusDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        // Spelled out rather than built with `support_audio_codec!`:
        // the macro expands to absolute `symphonia_core::` paths, and
        // this crate depends on the `symphonia` facade rather than on
        // `symphonia-core` under its own name.
        &[SupportedAudioCodec {
            id: CODEC_ID_OPUS,
            info: CodecInfo {
                short_name: "opus",
                long_name: "Opus",
                profiles: &[],
            },
        }]
    }
}

/// Positions for the common layouts, falling back to discrete channels
/// for anything else.
///
/// A wrong *name* for a channel is a worse failure than no name: the
/// resampler and the mixer downstream key on position, so guessing at
/// a 7-channel layout would quietly send a surround channel to a
/// tweeter. Discrete says "these are N channels in file order", which
/// is exactly what is known.
fn channel_layout(count: u8) -> Channels {
    match count {
        1 => CHANNEL_LAYOUT_MONO,
        2 => CHANNEL_LAYOUT_STEREO,
        other => Channels::Discrete(other.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal family-0 `OpusHead`: magic, version, channels,
    /// pre-skip, input rate, output gain, family.
    fn head(channels: u8, gain: i16, family: u8) -> Vec<u8> {
        let mut out = b"OpusHead".to_vec();
        out.push(1); // version
        out.push(channels);
        out.extend_from_slice(&312u16.to_le_bytes()); // pre-skip
        out.extend_from_slice(&48_000u32.to_le_bytes());
        out.extend_from_slice(&gain.to_le_bytes());
        out.push(family);
        out
    }

    #[test]
    fn a_stereo_header_is_one_coupled_stream() {
        let (parsed, mapping) = parse_head(&head(2, 0, 0)).expect("parse");
        assert_eq!(parsed.channel_count, 2);
        assert_eq!(mapping.streams, 1);
        assert_eq!(mapping.coupled_streams, 1);
        assert_eq!(mapping.mapping, vec![0, 1]);
    }

    #[test]
    fn a_mono_header_is_one_uncoupled_stream() {
        let (_, mapping) = parse_head(&head(1, 0, 0)).expect("parse");
        assert_eq!(mapping.streams, 1);
        assert_eq!(mapping.coupled_streams, 0);
        assert_eq!(mapping.mapping, vec![0]);
    }

    #[test]
    fn the_output_gain_is_read_as_signed() {
        let (parsed, _) = parse_head(&head(2, -512, 0)).expect("parse");
        assert_eq!(parsed.output_gain, -512, "-2 dB in Q7.8");
    }

    /// Family 1 carries its layout in a table after the header, and
    /// the table has to be there.
    #[test]
    fn a_surround_header_reads_its_mapping_table() {
        let mut bytes = head(6, 0, 1);
        bytes.push(4); // streams
        bytes.push(2); // coupled
        bytes.extend_from_slice(&[0, 4, 1, 2, 3, 5]);
        let (_, mapping) = parse_head(&bytes).expect("parse");
        assert_eq!(mapping.streams, 4);
        assert_eq!(mapping.coupled_streams, 2);
        assert_eq!(mapping.mapping, vec![0, 4, 1, 2, 3, 5]);
    }

    /// Everything that reaches libopus is checked first: a header that
    /// lies about its own size is a crash surface, not a decode error
    /// to discover later.
    #[test]
    fn a_truncated_mapping_table_is_refused() {
        let mut bytes = head(6, 0, 1);
        bytes.push(4);
        bytes.push(2);
        bytes.extend_from_slice(&[0, 4]); // four bytes short
        assert!(parse_head(&bytes).is_err());
    }

    #[test]
    fn an_impossible_stream_layout_is_refused() {
        for (streams, coupled) in [(0u8, 0u8), (2, 3)] {
            let mut bytes = head(2, 0, 255);
            bytes.push(streams);
            bytes.push(coupled);
            bytes.extend_from_slice(&[0, 1]);
            assert!(
                parse_head(&bytes).is_err(),
                "{streams} streams / {coupled} coupled should be refused"
            );
        }
    }

    #[test]
    fn family_zero_refuses_more_than_two_channels() {
        assert!(parse_head(&head(3, 0, 0)).is_err());
    }

    #[test]
    fn ambisonics_and_unknown_families_are_refused() {
        for family in [2u8, 3, 7, 128] {
            let mut bytes = head(2, 0, family);
            bytes.push(1);
            bytes.push(1);
            bytes.extend_from_slice(&[0, 1]);
            assert!(parse_head(&bytes).is_err(), "family {family}");
        }
    }

    #[test]
    fn a_header_that_is_not_one_is_refused() {
        assert!(parse_head(b"").is_err());
        assert!(parse_head(b"OpusTags\x01\x02").is_err());
        let short = &head(2, 0, 0)[..18];
        assert!(parse_head(short).is_err(), "one byte short of a header");
    }

    #[test]
    fn a_future_major_version_is_refused_but_a_minor_one_is_not() {
        let mut newer = head(2, 0, 0);
        newer[8] = 0x0F; // minor bump, still major 0
        assert!(parse_head(&newer).is_ok());
        newer[8] = 0x10;
        assert!(parse_head(&newer).is_err());
    }

    #[test]
    fn a_header_with_no_channels_is_refused() {
        assert!(parse_head(&head(0, 0, 0)).is_err());
    }

    /// The bug that would otherwise ship: 312 frames of encoder
    /// priming played at the head of every Opus track.
    ///
    /// `symphonia-format-ogg` reports a per-packet discard of zero for
    /// Opus — unlike Vorbis, where the reader does the counting — so
    /// this decoder counts the pre-skip itself. Measured end to end
    /// before this existed: a 3 s file decoded to 144 312 frames
    /// instead of 144 000.
    #[test]
    fn the_encoder_priming_is_dropped_from_the_head_of_the_stream() {
        let mut params = AudioCodecParameters::new();
        params.codec = CODEC_ID_OPUS;
        params.extra_data = Some(head(2, 0, 0).into_boxed_slice());
        let mut decoder =
            OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).expect("decoder");

        // A bare TOC selecting a 20 ms stereo CELT frame: 960 frames
        // of decoded audio per packet, and the header above declares a
        // 312-frame pre-skip.
        let packet = || {
            symphonia::core::packet::Packet::new(
                0,
                0.into(),
                960u64.into(),
                [0b1111_1100u8].as_slice(),
            )
        };

        let first = decoder.decode(&packet()).expect("decode").frames();
        assert_eq!(
            first,
            960 - 312,
            "the first packet is short by the pre-skip"
        );

        let second = decoder.decode(&packet()).expect("decode").frames();
        assert_eq!(second, 960, "and nothing is dropped after that");
    }

    /// A pre-skip longer than one packet is spent across as many as it
    /// takes, rather than clamped to the first.
    #[test]
    fn a_priming_longer_than_a_packet_spans_several() {
        let mut bytes = head(2, 0, 0);
        // 1500 frames of pre-skip: one 960-frame packet and part of
        // the next.
        bytes[10..12].copy_from_slice(&1500u16.to_le_bytes());
        let mut params = AudioCodecParameters::new();
        params.codec = CODEC_ID_OPUS;
        params.extra_data = Some(bytes.into_boxed_slice());
        let mut decoder =
            OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).expect("decoder");

        let packet = || {
            symphonia::core::packet::Packet::new(
                0,
                0.into(),
                960u64.into(),
                [0b1111_1100u8].as_slice(),
            )
        };
        assert_eq!(decoder.decode(&packet()).expect("decode").frames(), 0);
        assert_eq!(
            decoder.decode(&packet()).expect("decode").frames(),
            960 - 540
        );
        assert_eq!(decoder.decode(&packet()).expect("decode").frames(), 960);
    }

    /// A seek lands in the middle of a stream, where there is no
    /// priming to drop. Re-applying it would silently eat real audio
    /// on every seek.
    #[test]
    fn a_reset_does_not_re_apply_the_priming() {
        let mut params = AudioCodecParameters::new();
        params.codec = CODEC_ID_OPUS;
        params.extra_data = Some(head(2, 0, 0).into_boxed_slice());
        let mut decoder =
            OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).expect("decoder");

        let packet = || {
            symphonia::core::packet::Packet::new(
                0,
                0.into(),
                960u64.into(),
                [0b1111_1100u8].as_slice(),
            )
        };
        decoder.decode(&packet()).expect("decode");
        decoder.reset();
        assert_eq!(
            decoder.decode(&packet()).expect("decode").frames(),
            960,
            "nothing is owed after a seek"
        );
    }

    /// Gapless off means gapless off: a caller that has asked for
    /// every decoded frame gets the priming too, rather than a
    /// silently shortened stream.
    #[test]
    fn priming_is_kept_when_gapless_is_disabled() {
        let mut params = AudioCodecParameters::new();
        params.codec = CODEC_ID_OPUS;
        params.extra_data = Some(head(2, 0, 0).into_boxed_slice());
        let opts = AudioDecoderOptions::default().gapless(false);
        let mut decoder = OpusDecoder::try_new(&params, &opts).expect("decoder");

        let packet = symphonia::core::packet::Packet::new(
            0,
            0.into(),
            960u64.into(),
            [0b1111_1100u8].as_slice(),
        );
        assert_eq!(decoder.decode(&packet).expect("decode").frames(), 960);
    }

    /// The end-to-end shape, against the real library: build a decoder
    /// from a header we wrote, hand it a packet libopus produced, and
    /// get 48 kHz stereo frames back.
    #[test]
    fn a_real_packet_decodes_through_libopus() {
        let mut params = AudioCodecParameters::new();
        params.codec = CODEC_ID_OPUS;
        params.extra_data = Some(head(2, 0, 0).into_boxed_slice());

        let mut decoder = OpusDecoder::try_new(&params, &AudioDecoderOptions::default())
            .expect("a stereo decoder");

        // The shortest legal packet: a TOC byte selecting CELT
        // fullband 20 ms stereo, one frame, with no payload. libopus
        // decodes it as silence rather than refusing it.
        let toc = 0b1111_1100u8; // config 31, stereo, code 0
        let packet =
            symphonia::core::packet::Packet::new(0, 0.into(), 960u64.into(), [toc].as_slice());
        let decoded = decoder
            .decode(&packet)
            .expect("libopus should accept a bare TOC");
        assert_eq!(decoded.spec().rate(), OPUS_RATE);
        assert_eq!(decoded.spec().channels().count(), 2);
        assert!(decoded.frames() > 0, "a 20 ms frame is 960 samples");
    }

    #[test]
    fn a_corrupt_packet_is_an_error_and_leaves_no_audio_behind() {
        let mut params = AudioCodecParameters::new();
        params.codec = CODEC_ID_OPUS;
        params.extra_data = Some(head(2, 0, 0).into_boxed_slice());
        let mut decoder =
            OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).expect("decoder");

        // libopus reads an empty packet as packet loss and would
        // hand back concealment audio, so it is refused before it gets
        // there rather than being allowed to invent a frame.
        let empty = symphonia::core::packet::Packet::new(0, 0.into(), 960u64.into(), [].as_slice());
        assert!(
            decoder.decode(&empty).is_err(),
            "an empty packet is not one"
        );

        // And a packet that is data, but not Opus data.
        let junk = symphonia::core::packet::Packet::new(
            0,
            0.into(),
            960u64.into(),
            [0xFFu8; 3].as_slice(),
        );
        assert!(decoder.decode(&junk).is_err(), "a malformed packet");
        assert_eq!(
            decoder.last_decoded().frames(),
            0,
            "a failed decode must not leave the previous packet's audio readable"
        );
    }
}

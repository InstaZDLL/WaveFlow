//! AAC beyond what symphonia decodes, as a symphonia [`AudioDecoder`].
//!
//! symphonia's decoder covers AAC-LC, mono or stereo, with a plain
//! channel configuration, and refuses the rest when it is opened: more
//! than two channels ("aac too complex"), a program config element,
//! explicit SBR, Main, LTP, LD, ELD. A coupling channel or a program config
//! element inside a stereo stream it refuses at that packet instead. lofty
//! reads the tags of all of those perfectly well, so such a file entered
//! the library and failed the first time it was played.
//!
//! ## Who decodes what
//!
//! [`AacDecoder`] asks symphonia first and only takes what symphonia
//! refuses, so a stereo AAC file decodes exactly as it did before this
//! decoder existed. What comes here is `rusty_aac`.
//!
//! One thing symphonia would accept comes here too: SBR declared the
//! backward-compatible way (object type 2 plus a sync extension), which it
//! does not read and plays without its upper band. Measured on two ISO
//! streams: -3 and 23 dB against FFmpeg, 129.7 and 133.8 dB through here.
//!
//! SBR that nothing declares, in a stream declaring plain stereo AAC-LC,
//! stays with symphonia, which plays the core at half the rate. That is
//! measured (a 48 kHz file comes out at 24 kHz) and left alone here: seeing
//! it takes decoding, and going further means replacing symphonia's decoder
//! for every AAC file. `rusty_aac` itself does not rebuild the low-delay
//! SBR of AAC-ELD, whose core comes out at the core's rate.
//!
//! ## Why this crate
//!
//! Measured rather than read from READMEs: 22 files encoded by FFmpeg's
//! native encoder and by Media Foundation, plus the ISO/IEC 14496-26
//! streams of FFmpeg's FATE suite, each decoded sample by sample against
//! FFmpeg and, where FATE ships one, against its reference PCM.
//!
//! | Decoder         | Licence         | Result                                                                         |
//! | --------------- | --------------- | ------------------------------------------------------------------------------ |
//! | `rusty_aac`     | Apache-2.0      | ≥ 120 dB against FFmpeg on all but one stream, ≤ 2 LSB of 20 reference PCMs    |
//! | `syom`          | MIT             | LC right at 130 dB; every HE-AAC stream fails, Main and LTP refused            |
//! | `filmcraft-aac` | MIT, Apache-2.0 | LC only; HE-AAC decoded as its core at half the rate, without an error         |
//! | FAAD2 (binding) | GPL-2.0+        | Loses the first frame, gets 3.1 wrong (−2.9 dB), upmixes mono; binding is 2020 |
//! | FDK AAC, ports  | FDK licence     | Not measured: FFmpeg itself treats that licence as incompatible with the GPL   |
//!
//! The one stream below 120 dB, `aac-sce-in-stereo`, is a stereo
//! configuration carrying a single channel: FFmpeg leaves the second
//! channel uninitialised there, and it stays on symphonia anyway. Five
//! decoders agree at 132 dB or better once PNS is off; PNS is noise a
//! decoder invents, which is why two of them read 21-39 dB with it on.
//! Corrupted input measured too: 41 616 damaged packets and 200 000 random
//! configurations, with overflow checks on, caused no panic and no
//! non-finite sample. A damaged packet can produce a very large sample,
//! which the decoder thread's final clamp is there for.
//!
//! The version is pinned exactly: [`speakers`] reads the crate's public
//! layout tables, and this adapter was measured against this release.
//!
//! ## xHE-AAC
//!
//! USAC is a separate codec, and no candidate decodes it. It is refused
//! here, and [`crate::scanner::extract::decoder_channels`] turns that
//! refusal into a file the scanner does not list.

use rusty_aac::config::{self, aot, Pce, StreamConfig};
use rusty_aac::decode::layout::{self, OutputConfig};
use rusty_aac::decode::Decoder as AacCore;
use rusty_aac::BitReader;
use symphonia::core::audio::{
    layouts::{CHANNEL_LAYOUT_MONO, CHANNEL_LAYOUT_STEREO},
    AsGenericAudioBufferRef, Audio, AudioBuffer, AudioMut, AudioSpec, Channels,
    GenericAudioBufferRef, Position,
};
use symphonia::core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia::core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia::core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia::core::codecs::CodecInfo;
use symphonia::core::errors::{decode_error, unsupported_error, Error, Result};
use symphonia::core::packet::PacketRef;
use symphonia::default::codecs::AacDecoder as SymphoniaAac;

/// The positions FFmpeg's channel mask (which `rusty_aac` reports) and
/// symphonia's [`Position`] number alike: the 18 of the WAVE mask. Past
/// them the two go separate ways.
const SHARED_POSITIONS: u64 = (1 << 18) - 1;

/// Longest frame a packet decodes to: 1024 samples, doubled by SBR. The
/// buffer is sized for it once. It can still grow, as a defence: neither of
/// symphonia's readers hands over a packet of several frames.
const USUAL_MAX_FRAMES: usize = 2_048;

/// The registered AAC decoder: symphonia's for what it accepts,
/// [`ExtendedAac`] for what it refuses.
pub enum AacDecoder {
    Symphonia {
        decoder: Box<SymphoniaAac>,
        /// Whether a caller has read the format from this decoder yet.
        decoded_any: bool,
    },
    Extended(Box<ExtendedAac>),
}

impl AacDecoder {
    pub fn try_new(params: &AudioCodecParameters, opts: &AudioDecoderOptions) -> Result<Self> {
        // symphonia reads SBR only behind object type 5 or 29. Declared after
        // a plain object type 2, it would accept the stream and lose the band.
        let declares_sbr = params
            .extra_data
            .as_deref()
            .and_then(|config_bytes| config::parse(config_bytes).ok())
            .is_some_and(|stream| stream.sbr || stream.ext_rate_signalled);
        if declares_sbr {
            if let Ok(extended) = ExtendedAac::try_new(params, opts) {
                return Ok(Self::Extended(Box::new(extended)));
            }
        }
        if let Ok(decoder) = SymphoniaAac::try_new(params, opts) {
            return Ok(Self::Symphonia {
                decoder: Box::new(decoder),
                decoded_any: false,
            });
        }
        Ok(Self::Extended(Box::new(ExtendedAac::try_new(
            params, opts,
        )?)))
    }

    fn inner(&self) -> &dyn AudioDecoder {
        match self {
            Self::Symphonia { decoder, .. } => decoder.as_ref(),
            Self::Extended(decoder) => decoder.as_ref(),
        }
    }

    fn inner_mut(&mut self) -> &mut dyn AudioDecoder {
        match self {
            Self::Symphonia { decoder, .. } => decoder.as_mut(),
            Self::Extended(decoder) => decoder.as_mut(),
        }
    }
}

impl AudioDecoder for AacDecoder {
    fn reset(&mut self) {
        self.inner_mut().reset();
    }

    fn codec_info(&self) -> &CodecInfo {
        &Self::supported_codecs()
            .first()
            .expect("at least one codec registered")
            .info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        self.inner().codec_params()
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        if let Self::Symphonia {
            decoder,
            decoded_any,
        } = self
        {
            match decoder.decode_ref(packet) {
                Ok(_) => {
                    *decoded_any = true;
                    return Ok(self.last_decoded());
                }
                // Unsupported syntax in a stream it accepted: a coupling
                // channel, an in-band program config element. Fatal to a
                // caller, while the other decoder reads it.
                Err(Error::Unsupported(reason)) => {
                    let params = decoder.codec_params().clone();
                    let opts = AudioDecoderOptions::default();
                    let Ok(mut extended) = ExtendedAac::try_new(&params, &opts) else {
                        return Err(Error::Unsupported(reason));
                    };
                    extended.decoded_any = *decoded_any;
                    *self = Self::Extended(Box::new(extended));
                }
                Err(err) => return Err(err),
            }
        }
        self.inner_mut().decode_ref(packet)
    }

    fn finalize(&mut self) -> FinalizeResult {
        self.inner_mut().finalize()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.inner().last_decoded()
    }
}

impl RegisterableAudioDecoder for AacDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>> {
        Ok(Box::new(AacDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        // Spelled out for the reason `OpusDecoder` spells it out: the macro
        // expands to `symphonia_core::` paths this crate does not depend on.
        &[SupportedAudioCodec {
            id: CODEC_ID_AAC,
            info: CodecInfo {
                short_name: "aac",
                long_name: "Advanced Audio Coding",
                profiles: &[],
            },
        }]
    }
}

/// `rusty_aac` behind symphonia's decoder interface.
pub struct ExtendedAac {
    /// What a [`AudioDecoder::reset`] rebuilds the decoder from.
    stream: StreamConfig,
    decoder: rusty_aac::AacDecoder,
    params: AudioCodecParameters,
    opts: AudioDecoderOptions,
    /// The speakers the stream's configuration names, when it names any.
    speakers: Option<Channels>,
    /// Whether a caller has read the format yet. Until a packet has decoded,
    /// the buffer's format is a guess, and correcting it is not a change.
    /// Starts true when symphonia hands a stream over mid-way.
    decoded_any: bool,
    buf: AudioBuffer<f32>,
}

impl ExtendedAac {
    fn try_new(params: &AudioCodecParameters, opts: &AudioDecoderOptions) -> Result<Self> {
        let stream = match params.extra_data.as_deref() {
            Some(config_bytes) => config::parse(config_bytes).map_err(refused)?,
            // ADTS: the reader has consumed the headers, what they said is
            // in the parameters, and a configuration of 0 means the layout
            // arrives in-band.
            None => {
                let Some(rate) = params.sample_rate else {
                    return unsupported_error("aac: sample rate is required");
                };
                let configuration = params
                    .channels
                    .as_ref()
                    .map_or(0, |channels| channel_configuration(channels.count()));
                StreamConfig::lc(aot::AAC_LC, rate, configuration)
            }
        };
        // `AacDecoder` builds its core on the first packet. Built once here
        // too, so a configuration it cannot run is refused at the open like
        // every other decoder's, instead of failing packet after packet.
        AacCore::with_stream_config(stream.clone()).map_err(refused)?;

        let decoder = rusty_aac::AacDecoder::with_stream_config(stream.clone());
        let speakers = speakers(&stream);
        let rate = decoder
            .output_sample_rate()
            .or(params.sample_rate)
            .unwrap_or(stream.sample_rate);
        // The configuration's count first: symphonia's MP4 reader leaves the
        // channels empty for a stream with a program config element, and
        // Parametric Stereo makes two of the one it declares. Empty is what
        // stays when the layout arrives in-band: nothing is known yet, and
        // the buffer's stereo is a placeholder the first packet replaces.
        let declared = match (stream.ps, stream.channels()) {
            (true, _) => Some(CHANNEL_LAYOUT_STEREO),
            (false, 0) => params.channels.clone(),
            (false, count) => Some(channels_for(speakers.as_ref(), count)),
        };

        let mut params = params.clone();
        params.with_sample_rate(rate);
        if let Some(channels) = &declared {
            params.with_channels(channels.clone());
        }
        let channels = declared.unwrap_or(CHANNEL_LAYOUT_STEREO);

        Ok(Self {
            stream,
            decoder,
            params,
            opts: *opts,
            speakers,
            decoded_any: false,
            buf: AudioBuffer::new(AudioSpec::new(rate, channels), USUAL_MAX_FRAMES),
        })
    }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        self.buf.clear();

        let decoded = match self.decoder.decode(packet.data, None) {
            Ok(decoded) => decoded,
            Err(rusty_aac::Error::Again) => return decode_error("aac: empty packet"),
            Err(err) => {
                tracing::debug!(error = %err, "aac: packet refused");
                return decode_error("aac: malformed packet");
            }
        };
        let channels = usize::from(decoded.channels);
        if channels == 0 {
            return decode_error("aac: packet decoded to no channels");
        }
        let frames = decoded.frames();

        let same_format = self.buf.spec().rate() == decoded.sample_rate
            && self.buf.spec().channels().count() == channels;
        if !same_format || frames > self.buf.capacity() {
            if let Some(in_band) = in_band_speakers(packet.data) {
                self.speakers = Some(in_band);
            }
            let layout = channels_for(self.speakers.as_ref(), channels);
            self.buf = AudioBuffer::new(
                AudioSpec::new(decoded.sample_rate, layout.clone()),
                frames.max(USUAL_MAX_FRAMES),
            );
            self.params
                .with_sample_rate(decoded.sample_rate)
                .with_channels(layout);
            // A caller has already read the format from an earlier packet.
            // symphonia's way of saying that format no longer holds; this
            // packet's audio is the price, once.
            if !same_format && self.decoded_any {
                return Err(Error::ResetRequired);
            }
        }
        self.decoded_any = true;

        self.buf.render_uninit(Some(frames));
        for channel in 0..channels {
            let Some(plane) = self.buf.plane_mut(channel) else {
                return decode_error("aac: missing audio plane");
            };
            for (frame, sample) in plane.iter_mut().enumerate().take(frames) {
                *sample = decoded.samples[frame * channels + channel];
            }
        }

        if self.opts.gapless {
            // Bounded by what was decoded, as in `OpusDecoder`: a reader
            // must not be able to ask for a trim past the buffer's end.
            let start = (packet.trim_start.get() as usize).min(frames);
            let end = (packet.trim_end.get() as usize).min(frames - start);
            self.buf.trim(start, end);
        }
        Ok(())
    }
}

impl AudioDecoder for ExtendedAac {
    fn reset(&mut self) {
        // `rusty_aac` has no reset. After a seek the overlap it holds comes
        // from a part of the stream that no longer precedes the next
        // packet, so it is rebuilt; the configuration was accepted at the
        // open, and the format stays what the caller already read.
        self.decoder = rusty_aac::AacDecoder::with_stream_config(self.stream.clone());
    }

    fn codec_info(&self) -> &CodecInfo {
        &AacDecoder::supported_codecs()
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
                // The trait requires an empty buffer after a failure.
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

/// The refusal of a stream neither decoder takes. One message, since
/// symphonia's error carries a static string: the crate's own reason goes
/// to the log.
fn refused(err: rusty_aac::Error) -> Error {
    tracing::debug!(error = %err, "aac: stream refused");
    Error::Unsupported("aac: no decoder for this stream (xHE-AAC, for one)")
}

/// The `channel_configuration` of an ADTS stream of `count` channels.
/// Configuration 7 holds eight channels, and seven channels have no
/// configuration of their own: 0 lets the decoder learn them from a PCE.
fn channel_configuration(count: usize) -> u8 {
    match count {
        1..=6 => count as u8,
        8 => 7,
        _ => 0,
    }
}

/// The layout of `count` channels: the configuration's when it has that
/// many, the usual one otherwise. An in-band PCE or Parametric Stereo can
/// change the count from what was declared.
fn channels_for(speakers: Option<&Channels>, count: usize) -> Channels {
    match (speakers, count) {
        (Some(speakers), _) if speakers.count() == count => speakers.clone(),
        (_, 1) => CHANNEL_LAYOUT_MONO,
        (_, 2) => CHANNEL_LAYOUT_STEREO,
        _ => Channels::Discrete(count as u16),
    }
}

/// The speakers of a program config element that opens `packet`, which is
/// where an ADTS stream of configuration 0 keeps them.
fn in_band_speakers(packet: &[u8]) -> Option<Channels> {
    /// `ID_PCE`, the element type ISO/IEC 14496-3 gives a program config.
    const PROGRAM_CONFIG_ELEMENT: u32 = 5;

    let mut reader = BitReader::new(packet);
    if reader.read_bits(3).ok()? != PROGRAM_CONFIG_ELEMENT {
        return None;
    }
    let tag = reader.read_u8(4).ok()?;
    let pce = Pce::parse_after_tag(&mut reader, tag, 0).ok()?;
    speakers_of(layout::pce_layout(&pce))
}

/// The speakers `stream` configures, in the order `rusty_aac` outputs them:
/// it sorts channels by their bit in the channel mask, the order a
/// positional [`Channels`] means.
///
/// `None` when the stream leaves the layout to an in-band PCE, or when its
/// PCE names no speakers a mask can hold; the mask is then 0 and the output
/// keeps the PCE's own order. That is what FFmpeg writes for a layout with
/// no channel configuration (3.1, 4.1, 5.1 with side surrounds, 6.1, 7.1
/// wide): measured, its PCE files the LFE as a side channel, so the
/// bitstream cannot say which channel it is.
fn speakers(stream: &StreamConfig) -> Option<Channels> {
    speakers_of(match &stream.pce {
        Some(pce) => layout::pce_layout(pce),
        None => layout::default_layout(stream.channel_config, false)?,
    })
}

fn speakers_of(map: Vec<layout::Tag>) -> Option<Channels> {
    let output = OutputConfig::configure(map, false);
    let one_speaker_each = output.mask.count_ones() as usize == output.channels();
    if output.mask == 0 || !one_speaker_each || output.mask & !SHARED_POSITIONS != 0 {
        return None;
    }
    Position::from_bits(output.mask).map(Channels::Positioned)
}

#[cfg(test)]
mod tests;

//! Channel conversion between a track's layout and the output's.
//!
//! A [`ChannelLayout`] names the speaker behind each interleaved channel,
//! in the order the samples come. A source gets its layout from the
//! decoder when the decoder has one and from its channel count otherwise;
//! an output gets it from the backend that opened it. A [`ChannelMixer`]
//! is then the fixed list of gains from one layout to the other, worked
//! out once per track on the decoder thread.
//!
//! **The coefficients are ITU-R BS.775.** A channel the output has is
//! routed to it at unity. The centre folds into the front pair at −3 dB,
//! each surround into the front on its own side at −3 dB, and on a mono
//! output the front pair is −3 dB, the centre unity and the surrounds
//! −6 dB. The LFE is dropped when the output has none, as the standard
//! says: it carries nothing a full-range mix needs.
//!
//! **A surround is a surround.** A 5.1 file says "back" (FLAC, Vorbis,
//! Opus) or "side" (a WAVE mask) for the same pair of speakers, and a 5.1
//! output says one or the other depending on its driver. With one
//! surround pair on each end, that pair is routed at unity whichever word
//! is used, so 5.1 on 5.1 stays a copy. With 7.1 on 5.1, the side and back
//! of each side share the one speaker at −3 dB each, which keeps their
//! combined power; with 5.1 on 7.1, each pair goes to the speakers of its
//! own name and the other two stay silent. There is no upmix: a channel
//! the source does not have stays silent on the output.
//!
//! **A fold never clips.** The gains into one output channel can add up to
//! more than one, and a hot master would then reach the final clamp as
//! hard clipping. A mixer whose heaviest channel sums past unity is scaled
//! down as a whole, so the balance of the mix is what BS.775 says and the
//! fold plays quieter instead, as widening and an EQ boost do. Routing at
//! unity is left alone, bit for bit.

use symphonia::core::audio::Channels;

/// −3 dB, the step every fold in BS.775 is made of.
const K: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// One speaker position. The discriminants fit in a nibble: an output's
/// layout travels through one atomic, see [`ChannelLayout::pack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Speaker {
    FrontLeft = 1,
    FrontRight = 2,
    FrontCenter = 3,
    Lfe = 4,
    BackLeft = 5,
    BackRight = 6,
    FrontLeftOfCenter = 7,
    FrontRightOfCenter = 8,
    BackCenter = 9,
    SideLeft = 10,
    SideRight = 11,
    TopLeft = 12,
    TopRight = 13,
    TopCenter = 14,
    /// A channel with no position anyone could name.
    Unknown = 15,
}

impl Speaker {
    fn from_nibble(value: u8) -> Self {
        match value {
            1 => Self::FrontLeft,
            2 => Self::FrontRight,
            3 => Self::FrontCenter,
            4 => Self::Lfe,
            5 => Self::BackLeft,
            6 => Self::BackRight,
            7 => Self::FrontLeftOfCenter,
            8 => Self::FrontRightOfCenter,
            9 => Self::BackCenter,
            10 => Self::SideLeft,
            11 => Self::SideRight,
            12 => Self::TopLeft,
            13 => Self::TopRight,
            14 => Self::TopCenter,
            _ => Self::Unknown,
        }
    }

    /// The speaker a WAVE channel-mask bit stands for. The bit numbers are
    /// the ones of `dwChannelMask`, which symphonia's [`Position`] keeps.
    fn from_wave_bit(bit: u32) -> Self {
        match bit {
            0 => Self::FrontLeft,
            1 => Self::FrontRight,
            2 => Self::FrontCenter,
            3 => Self::Lfe,
            4 => Self::BackLeft,
            5 => Self::BackRight,
            6 => Self::FrontLeftOfCenter,
            7 => Self::FrontRightOfCenter,
            8 => Self::BackCenter,
            9 => Self::SideLeft,
            10 => Self::SideRight,
            12 | 15 => Self::TopLeft,
            14 | 17 => Self::TopRight,
            11 | 13 | 16 => Self::TopCenter,
            _ => Self::Unknown,
        }
    }

    /// The same-side partner of a left or right speaker.
    fn partner(self) -> Option<Self> {
        Some(match self {
            Self::FrontLeft => Self::FrontRight,
            Self::BackLeft => Self::BackRight,
            Self::SideLeft => Self::SideRight,
            Self::FrontLeftOfCenter => Self::FrontRightOfCenter,
            Self::TopLeft => Self::TopRight,
            Self::FrontRight => Self::FrontLeft,
            Self::BackRight => Self::BackLeft,
            Self::SideRight => Self::SideLeft,
            Self::FrontRightOfCenter => Self::FrontLeftOfCenter,
            Self::TopRight => Self::TopLeft,
            _ => return None,
        })
    }

    fn is_left(self) -> bool {
        matches!(
            self,
            Self::FrontLeft
                | Self::BackLeft
                | Self::SideLeft
                | Self::FrontLeftOfCenter
                | Self::TopLeft
        )
    }

    /// The speaker on the other surround pair, on the same side: where a
    /// surround goes when the output only has the other pair.
    fn other_surround(self) -> Option<Self> {
        Some(match self {
            Self::BackLeft => Self::SideLeft,
            Self::BackRight => Self::SideRight,
            Self::SideLeft => Self::BackLeft,
            Self::SideRight => Self::BackRight,
            _ => return None,
        })
    }
}

/// How many channels fit in a packed layout: one nibble each in a `u64`.
const PACKED_CHANNELS: usize = 16;

/// The speaker behind each interleaved channel, in the order they come.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelLayout {
    speakers: Vec<Speaker>,
}

impl ChannelLayout {
    /// A layout from its speakers, in the order of the samples.
    pub fn new(speakers: Vec<Speaker>) -> Self {
        Self { speakers }
    }

    /// The layout a bare channel count stands for, in WAVE order.
    ///
    /// Four channels are quadraphonic, the common 4-channel music layout.
    /// Past 7.1 the further channels have no position to give them.
    pub fn from_count(count: usize) -> Self {
        use Speaker::*;
        let mut speakers = match count {
            0 => Vec::new(),
            1 => vec![FrontCenter],
            2 => vec![FrontLeft, FrontRight],
            3 => vec![FrontLeft, FrontRight, FrontCenter],
            4 => vec![FrontLeft, FrontRight, BackLeft, BackRight],
            5 => vec![FrontLeft, FrontRight, FrontCenter, BackLeft, BackRight],
            6 => vec![FrontLeft, FrontRight, FrontCenter, Lfe, BackLeft, BackRight],
            7 => vec![
                FrontLeft,
                FrontRight,
                FrontCenter,
                Lfe,
                BackLeft,
                BackRight,
                BackCenter,
            ],
            _ => vec![
                FrontLeft,
                FrontRight,
                FrontCenter,
                Lfe,
                BackLeft,
                BackRight,
                SideLeft,
                SideRight,
            ],
        };
        speakers.resize(count.max(speakers.len()), Unknown);
        Self { speakers }
    }

    /// The default order of an ALSA PCM, which is not WAVE's: the rear pair
    /// comes before the centre and the LFE. Used where a device says nothing
    /// of its own. The PipeWire ALSA plugin answers exactly this for every
    /// count, and maps the channels accordingly (measured against an
    /// 8-channel sink).
    pub fn alsa_default(count: usize) -> Self {
        use Speaker::*;
        match count {
            5 => Self::new(vec![
                FrontLeft,
                FrontRight,
                BackLeft,
                BackRight,
                FrontCenter,
            ]),
            6 => Self::new(vec![
                FrontLeft,
                FrontRight,
                BackLeft,
                BackRight,
                FrontCenter,
                Lfe,
            ]),
            8 => Self::new(vec![
                FrontLeft,
                FrontRight,
                BackLeft,
                BackRight,
                FrontCenter,
                Lfe,
                SideLeft,
                SideRight,
            ]),
            _ => Self::from_count(count),
        }
    }

    /// The layout of a WAVE channel mask: one channel per set bit, lowest
    /// first, which is how the samples of such a stream are ordered. A mask
    /// that does not have `count` bits says nothing about these channels
    /// and gives way to the count.
    pub fn from_wave_mask(mask: u32, count: usize) -> Self {
        if mask.count_ones() as usize != count {
            return Self::from_count(count);
        }
        Self {
            speakers: (0..u32::BITS)
                .filter(|bit| mask & (1 << bit) != 0)
                .map(Speaker::from_wave_bit)
                .collect(),
        }
    }

    /// The layout of what a decoder reported. Only positioned channels say
    /// where they go; anything else (discrete, ambisonic, labelled) falls
    /// back on the count. A single channel is mono whichever position it
    /// was given: FLAC and Vorbis say front left, others front centre.
    pub fn from_symphonia(channels: &Channels) -> Self {
        let count = channels.count();
        match channels {
            Channels::Positioned(positions) if count > 1 => {
                match u32::try_from(positions.bits()) {
                    Ok(mask) => Self::from_wave_mask(mask, count),
                    // A position past the WAVE mask's 32 bits has no name here.
                    Err(_) => Self::from_count(count),
                }
            }
            _ => Self::from_count(count),
        }
    }

    pub fn channels(&self) -> usize {
        self.speakers.len()
    }

    pub fn speakers(&self) -> &[Speaker] {
        &self.speakers
    }

    fn index_of(&self, speaker: Speaker) -> Option<usize> {
        self.speakers.iter().position(|s| *s == speaker)
    }

    fn has(&self, speaker: Speaker) -> bool {
        self.index_of(speaker).is_some()
    }

    /// Whether both surround pairs are there, as on 7.1.
    fn has_two_surround_pairs(&self) -> bool {
        [
            Speaker::BackLeft,
            Speaker::BackRight,
            Speaker::SideLeft,
            Speaker::SideRight,
        ]
        .iter()
        .all(|s| self.has(*s))
    }

    /// The channel indices of every left / right pair, so that a stage
    /// working on pairs reaches the surrounds as well as the front. Left
    /// first in each.
    pub fn pairs(&self) -> Vec<(usize, usize)> {
        self.speakers
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_left())
            .filter_map(|(left, s)| Some((left, self.index_of(s.partner()?)?)))
            .collect()
    }

    /// The indices of the front pair, when the layout has one.
    pub fn front_pair(&self) -> Option<(usize, usize)> {
        Some((
            self.index_of(Speaker::FrontLeft)?,
            self.index_of(Speaker::FrontRight)?,
        ))
    }

    /// The gain of each channel in a fold to mono: BS.775's, scaled so the
    /// gains add up to one and a full-scale frame stays in range.
    ///
    /// `source_channels` is how many channels the track has. A stereo or
    /// mono track on a wider output only uses the front pair, and folding
    /// it with the gains of a mix that fills every speaker would take
    /// 13 dB off it for nothing.
    pub fn mono_gains(&self, source_channels: usize) -> Vec<f32> {
        if source_channels <= 2 {
            if let Some((left, right)) = self.front_pair() {
                let mut gains = vec![0.0; self.channels()];
                gains[left] = 0.5;
                gains[right] = 0.5;
                return gains;
            }
        }
        let mono = Self::from_count(1);
        let mixer = ChannelMixer::new(self, &mono);
        let mut gains = vec![0.0; self.channels()];
        match mixer.routes {
            Some(routes) => {
                for route in routes {
                    gains[route.src] = route.gain;
                }
            }
            None => gains.fill(1.0),
        }
        gains
    }

    /// The layout as one `u64`, one nibble per channel from the low end, so
    /// an output's layout can sit next to its other atomics in the shared
    /// state. Zero when it does not fit, which reads back as the count's.
    pub fn pack(&self) -> u64 {
        if self.speakers.len() > PACKED_CHANNELS {
            return 0;
        }
        self.speakers
            .iter()
            .enumerate()
            .fold(0, |packed, (i, s)| packed | ((*s as u64) << (i * 4)))
    }

    /// The layout of an output with `count` channels, from what [`Self::pack`]
    /// left. A value that does not hold `count` channels (zero, or an output
    /// that has since changed) gives way to the count.
    pub fn unpack(packed: u64, count: usize) -> Self {
        let held = (0..PACKED_CHANNELS)
            .take_while(|i| (packed >> (i * 4)) & 0xF != 0)
            .count();
        if held != count {
            return Self::from_count(count);
        }
        Self {
            speakers: (0..count)
                .map(|i| Speaker::from_nibble(((packed >> (i * 4)) & 0xF) as u8))
                .collect(),
        }
    }
}

/// One gain of a mixer: this much of source channel `src` into output
/// channel `dst`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Route {
    src: usize,
    dst: usize,
    gain: f32,
}

/// The fixed gains from one layout to another.
#[derive(Debug, Clone)]
pub struct ChannelMixer {
    src: ChannelLayout,
    src_channels: usize,
    dst_channels: usize,
    /// `None` when the two layouts are the same: the samples are copied.
    routes: Option<Vec<Route>>,
}

impl ChannelMixer {
    pub fn new(src: &ChannelLayout, dst: &ChannelLayout) -> Self {
        let (src_channels, dst_channels) = (src.channels(), dst.channels());
        if src == dst {
            return Self {
                src: src.clone(),
                src_channels,
                dst_channels,
                routes: None,
            };
        }
        let mut routes = Vec::new();
        if src_channels == 1 {
            mono_routes(dst, &mut routes);
        } else {
            for (i, speaker) in src.speakers().iter().enumerate() {
                speaker_routes(*speaker, i, src, dst, &mut routes);
            }
        }
        normalize(&mut routes, dst_channels);
        Self {
            src: src.clone(),
            src_channels,
            dst_channels,
            routes: Some(routes),
        }
    }

    /// Whether this mixer was built for a source of this layout.
    pub fn is_from(&self, src: &ChannelLayout) -> bool {
        self.src == *src
    }

    /// Mix interleaved `src`-wide samples onto the end of `out`, interleaved
    /// `dst`-wide.
    pub fn convert(&self, input: &[f32], out: &mut Vec<f32>) {
        if self.src_channels == 0 || self.dst_channels == 0 {
            return;
        }
        let Some(routes) = &self.routes else {
            out.extend_from_slice(input);
            return;
        };
        let frames = input.len() / self.src_channels;
        let start = out.len();
        out.resize(start + frames * self.dst_channels, 0.0);
        for (from, to) in input
            .chunks_exact(self.src_channels)
            .zip(out[start..].chunks_exact_mut(self.dst_channels))
        {
            for route in routes {
                to[route.dst] += from[route.src] * route.gain;
            }
        }
    }
}

/// A mono source plays from the front pair at full level on any output that
/// has one, exactly as it does on a stereo output: a centre speaker is not
/// something every 5.1 setup is matched for.
fn mono_routes(dst: &ChannelLayout, routes: &mut Vec<Route>) {
    let mut to = |dst: usize| {
        routes.push(Route {
            src: 0,
            dst,
            gain: 1.0,
        })
    };
    match dst.front_pair() {
        Some((left, right)) => {
            to(left);
            to(right);
        }
        None => to(dst.index_of(Speaker::FrontCenter).unwrap_or(0)),
    }
}

/// The routes of one source channel.
fn speaker_routes(
    speaker: Speaker,
    src: usize,
    source: &ChannelLayout,
    output: &ChannelLayout,
    routes: &mut Vec<Route>,
) {
    use Speaker::*;
    let mut add = |dst: usize, gain: f32| routes.push(Route { src, dst, gain });
    // The front on the same side, for what folds into it.
    let front_of = |left: bool| output.index_of(if left { FrontLeft } else { FrontRight });
    let centre = output.index_of(FrontCenter);
    let front = output.front_pair();

    if matches!(speaker, BackLeft | BackRight | SideLeft | SideRight) {
        // Two surround pairs sharing the output's one: −3 dB each, so
        // together they keep the power of one.
        let merged = source.has_two_surround_pairs() && !output.has_two_surround_pairs();
        let shared = if merged { K } else { 1.0 };
        if let Some(dst) = output.index_of(speaker) {
            add(dst, shared);
        } else if let Some(dst) = speaker.other_surround().and_then(|s| output.index_of(s)) {
            add(dst, shared);
        } else if let Some(dst) = output.index_of(BackCenter) {
            add(dst, K);
        } else if let Some(dst) = front_of(speaker.is_left()) {
            add(dst, K);
        } else if let Some(dst) = centre {
            add(dst, 0.5);
        }
        return;
    }

    if let Some(dst) = output.index_of(speaker) {
        add(dst, 1.0);
        return;
    }

    match speaker {
        // Dropped where there is none to send it to.
        Lfe => {}
        FrontCenter => {
            if let Some((left, right)) = front {
                add(left, K);
                add(right, K);
            }
        }
        FrontLeft | FrontRight => {
            if let Some(dst) = centre {
                add(dst, K);
            }
        }
        FrontLeftOfCenter | FrontRightOfCenter | TopLeft | TopRight => {
            if let Some(dst) = front_of(speaker.is_left()) {
                add(dst, K);
            } else if let Some(dst) = centre {
                add(dst, 0.5);
            }
        }
        BackCenter => {
            let pair = [(BackLeft, BackRight), (SideLeft, SideRight)]
                .into_iter()
                .find_map(|(l, r)| Some((output.index_of(l)?, output.index_of(r)?)))
                .or(front);
            if let Some((left, right)) = pair {
                add(left, K);
                add(right, K);
            } else if let Some(dst) = centre {
                add(dst, K);
            }
        }
        TopCenter => {
            if let Some((left, right)) = front {
                add(left, 0.5);
                add(right, 0.5);
            } else if let Some(dst) = centre {
                add(dst, 0.5);
            }
        }
        // A channel nobody can place still is not dropped: past 7.1 they
        // fold alternately left and right, by their position in the frame.
        Unknown => {
            if let Some(dst) = front_of(src % 2 == 0) {
                add(dst, K);
            } else if let Some(dst) = centre {
                add(dst, 0.5);
            }
        }
        // Handled above: the output has them, or they went to a surround.
        BackLeft | BackRight | SideLeft | SideRight => {}
    }
}

/// Scale the whole mix down if any output channel can sum past unity.
fn normalize(routes: &mut [Route], dst_channels: usize) {
    let mut sums = vec![0.0_f32; dst_channels];
    for route in routes.iter() {
        sums[route.dst] += route.gain;
    }
    let heaviest = sums.into_iter().fold(0.0, f32::max);
    if heaviest > 1.0 {
        for route in routes.iter_mut() {
            route.gain /= heaviest;
        }
    }
}

#[cfg(test)]
mod tests;

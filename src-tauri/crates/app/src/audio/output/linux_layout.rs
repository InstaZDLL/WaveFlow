//! What Linux does not tell a stream about its channels.
//!
//! cpal's default configuration prefers stereo above anything else, and the
//! sound server's ALSA plugin offers every count from 1 to 128, so a shared
//! stream was stereo on every Linux desktop, a 7.1 sink included. The sink's
//! real width is known to the sound server alone, and `pactl` is how to ask
//! either PipeWire or PulseAudio without linking to one of them. Without it
//! the stream stays stereo, as it always was.
//!
//! That plugin and a raw `hw:` device share an order that is not WAVE's (see
//! [`ChannelLayout::alsa_default`]), which `hw:` can state for itself through
//! its channel map.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use alsa::pcm::{Chmap, ChmapPosition};
use cpal::traits::DeviceTrait;
use cpal::SupportedStreamConfig;
use waveflow_core::playback::channels::{ChannelLayout, Speaker};

/// How long `pactl` may take. It answers in milliseconds or the server is
/// not there, and an output must not wait on it.
const PACTL_TIMEOUT: Duration = Duration::from_millis(500);

/// The widest sink opened at its own width: 7.1, the widest layout with names.
const MAX_SINK_CHANNELS: u16 = 8;

/// The ALSA device names that lead to the sound server rather than to a card.
const SERVER_DEVICES: [&str; 3] = ["default", "pipewire", "pulse"];

/// Open as wide as the sink the stream will land on, when that is the sound
/// server and the device can be asked for that many channels.
pub(in crate::audio) fn widen_to_sink(
    device: &cpal::Device,
    device_name: Option<&str>,
    default: SupportedStreamConfig,
) -> SupportedStreamConfig {
    if device_name.is_some_and(|name| !SERVER_DEVICES.contains(&name)) {
        return default;
    }
    let Some(channels) = sink_channels().filter(|n| *n > default.channels()) else {
        return default;
    };
    let rate = default.sample_rate();
    let wider = device
        .supported_output_configs()
        .ok()
        .and_then(|mut configs| {
            configs.find(|c| {
                c.channels() == channels
                    && c.sample_format() == default.sample_format()
                    && c.min_sample_rate() <= rate
                    && rate <= c.max_sample_rate()
            })
        });
    match wider {
        Some(config) => {
            tracing::info!(channels, "opening at the width of the sound server's sink");
            config.with_sample_rate(rate)
        }
        None => default,
    }
}

/// How many speakers the sink has that a stream of ours would land on: the
/// one the ALSA plugin was pointed at through the environment, or the default.
///
/// `None` past 7.1. A sink that wide is a card's "Pro Audio" profile, whose
/// channels are numbered inputs of a mixing desk rather than speakers.
pub(in crate::audio) fn sink_channels() -> Option<u16> {
    let sink = std::env::var("PIPEWIRE_NODE")
        .or_else(|_| std::env::var("PULSE_SINK"))
        .ok()
        .filter(|name| !name.is_empty())
        .or_else(|| {
            let default = pactl(&["get-default-sink"])?;
            Some(default.trim().to_string())
        })?;
    channels_of_sink(&pactl(&["list", "short", "sinks"])?, &sink)
        .filter(|n| *n <= MAX_SINK_CHANNELS)
}

/// One line of `pactl list short sinks` per sink, tab-separated and free of
/// any translated word: index, name, driver, sample spec (`s32le 2ch 48000Hz`),
/// state.
fn channels_of_sink(listing: &str, sink: &str) -> Option<u16> {
    listing.lines().find_map(|line| {
        let mut fields = line.split('\t');
        if fields.nth(1)? != sink {
            return None;
        }
        fields
            .nth(1)?
            .split_whitespace()
            .find_map(|word| word.strip_suffix("ch")?.parse().ok())
    })
}

/// Run `pactl`, giving up after [`PACTL_TIMEOUT`]. `None` for anything but a
/// clean answer: no such tool (a Flatpak runtime may not ship it), no server.
fn pactl(args: &[&str]) -> Option<String> {
    let mut child = Command::new("pactl")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait().ok()? {
            Some(status) if status.success() => break,
            Some(_) => return None,
            None if started.elapsed() > PACTL_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let output = child.wait_with_output().ok()?;
    String::from_utf8(output.stdout).ok()
}

/// The layout a raw device stated through its channel map.
pub(in crate::audio) fn layout_of_chmap(map: &Chmap) -> Option<ChannelLayout> {
    let positions: Vec<ChmapPosition> = map.into();
    layout_of_positions(&positions)
}

/// `None` when any channel has no position (a device that does not know), so
/// the caller falls back rather than treat part of the map as gospel.
fn layout_of_positions(positions: &[ChmapPosition]) -> Option<ChannelLayout> {
    use ChmapPosition as P;
    let speakers = positions
        .iter()
        .map(|position| {
            Some(match position {
                P::Mono | P::FC => Speaker::FrontCenter,
                P::FL => Speaker::FrontLeft,
                P::FR => Speaker::FrontRight,
                P::LFE => Speaker::Lfe,
                P::RL => Speaker::BackLeft,
                P::RR => Speaker::BackRight,
                P::RC => Speaker::BackCenter,
                P::SL => Speaker::SideLeft,
                P::SR => Speaker::SideRight,
                P::FLC => Speaker::FrontLeftOfCenter,
                P::FRC => Speaker::FrontRightOfCenter,
                _ => return None,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    (!speakers.is_empty()).then(|| ChannelLayout::new(speakers))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str =
        "47\talsa_output.pci-0000_00_1f.3.analog-stereo\tPipeWire\ts32le 2ch 48000Hz\tSUSPENDED\n\
                           51\twf_mc8\tPipeWire\tfloat32le 8ch 48000Hz\tRUNNING\n";

    #[test]
    fn the_width_is_read_off_the_named_sink() {
        assert_eq!(
            channels_of_sink(LISTING, "alsa_output.pci-0000_00_1f.3.analog-stereo"),
            Some(2)
        );
        assert_eq!(channels_of_sink(LISTING, "wf_mc8"), Some(8));
        assert_eq!(channels_of_sink(LISTING, "gone"), None);
        assert_eq!(channels_of_sink("", "wf_mc8"), None);
        assert_eq!(channels_of_sink("47\twf_mc8\n", "wf_mc8"), None);
    }

    /// What an HDA card's 5.1 states: the rear pair before the centre.
    #[test]
    fn a_channel_map_keeps_the_device_order() {
        use ChmapPosition as P;
        let layout = layout_of_positions(&[P::FL, P::FR, P::RL, P::RR, P::FC, P::LFE]).unwrap();
        assert_eq!(layout, ChannelLayout::alsa_default(6));
    }

    #[test]
    fn a_map_with_an_unknown_channel_says_nothing() {
        use ChmapPosition as P;
        assert_eq!(layout_of_positions(&[P::FL, P::Unknown]), None);
        assert_eq!(layout_of_positions(&[P::NA, P::NA]), None);
        assert_eq!(layout_of_positions(&[]), None);
    }
}

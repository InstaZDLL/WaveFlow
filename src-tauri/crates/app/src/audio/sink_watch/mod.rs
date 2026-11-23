//! Pause when the device under our stream goes away, on Linux (#1158).
//!
//! On Windows and macOS a device that disappears breaks the stream: cpal
//! reports it, and [`super::output::notify_device_lost`] and the rebuild
//! that follows decide what happens to the music. Under PipeWire or
//! PulseAudio it does not break anything. cpal opens the ALSA `default`
//! alias, which is a client of the sound server, and the server moves that
//! client to another sink by itself — measured: unplugging the sink under
//! a playing ALSA stream leaves the stream writing without a single error.
//! So `audio.pause_on_device_loss` never fired, and the music carried on
//! out of the speakers.
//!
//! The only party that knows is the server, so this module asks it. It
//! speaks the PulseAudio native protocol — which pipewire-pulse serves on
//! PipeWire systems, and PulseAudio itself everywhere else — through a
//! pure-Rust client, so it needs no system library. One thread subscribes
//! to sink, stream and card events; on each one it reads the server's
//! state on a second connection and hands it to [`decision::Tracker`],
//! which says whether the device one of our streams played through went
//! away (the rules, and what was measured, are in [`decision`]).
//!
//! Our streams are the server's streams whose client carries our process
//! id in `application.process.id`, a property the ALSA plugin fills from
//! inside the process — so it matches inside a Flatpak sandbox too, where
//! the server's own idea of the pid does not.
//!
//! **What a loss does.** Nothing to the output: the stream is healthy, it
//! is only somewhere else, so there is nothing to rebuild. When the
//! setting is on and something is playing, it pauses — through
//! [`crate::player_actions::pause`], the same pause the media keys send,
//! which also reaches an open music video — and posts the
//! [`PlaybackNotice::PausedDeviceLost`] notice the rebuild posts on the
//! other platforms. Play then resumes on the device the server chose,
//! exactly as it does there after a park.
//!
//! **Fail-soft.** No server, a refused connection, a server that restarts:
//! the thread logs, waits and tries again, and the app plays exactly as it
//! did before this existed. Nothing here touches the audio callback or the
//! decoder.

pub mod decision;

use std::ffi::CString;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use pulseaudio::protocol::{
    self, port_info::PortAvailable, Command, CommandReply, Prop, SubscriptionMask,
};
use tauri::{AppHandle, Manager};

use super::engine::PlaybackNotice;
use super::{AudioEngine, PlayerState};
use decision::{CardView, Loss, PortState, SinkView, Snapshot, StreamView, Tracker};

/// First wait before reconnecting; doubled after each failure up to
/// [`RECONNECT_MAX`], and reset by a connection that worked.
const RECONNECT_MIN: Duration = Duration::from_secs(5);

/// A machine without a sound server never grows one, and a restarted one
/// is back within seconds: past five minutes, waiting longer costs
/// nothing.
const RECONNECT_MAX: Duration = Duration::from_secs(300);

/// Start the watch. Called once from [`super::default_device::spawn`].
pub fn spawn(app: AppHandle) {
    let spawned = std::thread::Builder::new()
        .name("sink-watch".into())
        .spawn(move || run(&app));
    if let Err(err) = spawned {
        tracing::warn!(%err, "sink watch: could not start its thread");
    }
}

fn run(app: &AppHandle) {
    let mut delay = RECONNECT_MIN;
    let mut said_why = false;
    loop {
        match watch(app) {
            // `watch` only returns once a connection it had broke.
            Ok(()) => delay = RECONNECT_MIN,
            Err(err) if !said_why => {
                tracing::info!(
                    %err,
                    "sink watch: no PulseAudio-compatible sound server to ask; a device \
                     unplugged under the stream will not pause playback"
                );
                said_why = true;
            }
            Err(err) => tracing::debug!(%err, "sink watch: still no sound server"),
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(RECONNECT_MAX);
    }
}

/// One connection's life: subscribe, take the first picture, then judge
/// every relevant event until the server goes away. Returns `Ok` when an
/// established connection breaks, `Err` when none could be made.
fn watch(app: &AppHandle) -> Result<(), protocol::ProtocolError> {
    let mut events = Connection::open()?;
    let mut queries = Connection::open()?;
    events.subscribe(
        SubscriptionMask::SINK | SubscriptionMask::SINK_INPUT | SubscriptionMask::CARD,
    )?;
    tracing::info!("sink watch: following the sound server's sinks");

    let pid = std::process::id();
    let mut tracker = Tracker::default();
    // The first picture is the baseline: nothing is judged against it.
    tracker.observe(&queries.snapshot(pid)?);

    loop {
        // Only the three subscribed kinds arrive. One unplug sends a
        // burst of them, and each gets its own picture: a local query
        // costs little, and the removal and the move are often two
        // events apart.
        if let Err(err) = events.next_event() {
            tracing::info!(%err, "sink watch: lost the sound server; reconnecting");
            return Ok(());
        }
        let snapshot = match queries.snapshot(pid) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                tracing::info!(%err, "sink watch: the sound server stopped answering; reconnecting");
                return Ok(());
            }
        };
        if let Some(loss) = tracker.observe(&snapshot) {
            on_loss(app, loss);
        }
    }
}

/// The device under our stream went away and the server moved it.
fn on_loss(app: &AppHandle, loss: Loss) {
    let Some(engine) = app.try_state::<Arc<AudioEngine>>() else {
        return;
    };
    if !engine.pause_on_device_loss() {
        tracing::info!(
            ?loss,
            "sink watch: the output device went away; pausing is off"
        );
        return;
    }
    let playing =
        engine.shared().state() == PlayerState::Playing || crate::player_actions::clip_is_open();
    if !playing {
        tracing::debug!(
            ?loss,
            "sink watch: the output device went away while nothing played"
        );
        return;
    }
    tracing::info!(
        ?loss,
        "the output device went away and the sound server moved the stream; pausing playback"
    );
    crate::player_actions::pause(app);
    engine.emit_playback_notice(PlaybackNotice::PausedDeviceLost);
}

/// One connection to the server, request and reply in lockstep. Two are
/// opened because a subscribed connection interleaves events with
/// replies, and reading them apart is more code than a second socket.
struct Connection {
    socket: BufReader<UnixStream>,
    version: u16,
    seq: u32,
}

impl Connection {
    fn open() -> Result<Self, protocol::ProtocolError> {
        let path = pulseaudio::socket_path_from_env().ok_or_else(|| {
            protocol::ProtocolError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no sound server socket",
            ))
        })?;
        let mut socket = BufReader::new(UnixStream::connect(path)?);
        // pipewire-pulse accepts any cookie from the same user; PulseAudio
        // wants the one it wrote, when it wrote one.
        let cookie = pulseaudio::cookie_path_from_env()
            .and_then(|path| std::fs::read(path).ok())
            .unwrap_or_default();
        let auth = protocol::AuthParams {
            version: protocol::MAX_VERSION,
            supports_shm: false,
            supports_memfd: false,
            cookie,
        };
        protocol::write_command_message(
            socket.get_mut(),
            0,
            &Command::Auth(auth),
            protocol::MAX_VERSION,
        )?;
        let (_, reply) = protocol::read_reply_message::<protocol::AuthReply>(
            &mut socket,
            protocol::MAX_VERSION,
        )?;
        let mut connection = Self {
            socket,
            version: protocol::MAX_VERSION.min(reply.version),
            seq: 0,
        };
        let mut props = protocol::Props::new();
        props.set(Prop::ApplicationName, c"WaveFlow");
        connection.request::<protocol::SetClientNameReply>(Command::SetClientName(props))?;
        Ok(connection)
    }

    fn send(&mut self, command: &Command) -> Result<u32, protocol::ProtocolError> {
        self.seq = self.seq.wrapping_add(1);
        protocol::write_command_message(self.socket.get_mut(), self.seq, command, self.version)?;
        Ok(self.seq)
    }

    fn request<T: CommandReply>(&mut self, command: Command) -> Result<T, protocol::ProtocolError> {
        self.send(&command)?;
        let (_, reply) = protocol::read_reply_message::<T>(&mut self.socket, self.version)?;
        Ok(reply)
    }

    fn subscribe(&mut self, mask: SubscriptionMask) -> Result<(), protocol::ProtocolError> {
        self.send(&Command::Subscribe(mask))?;
        protocol::read_ack_message(&mut self.socket)?;
        Ok(())
    }

    /// Block until the server sends the next subscription event.
    /// Anything else it volunteers is skipped.
    fn next_event(&mut self) -> Result<(), protocol::ProtocolError> {
        loop {
            let (_, command) = protocol::read_command_message(&mut self.socket, self.version)?;
            if matches!(command, Command::SubscribeEvent(_)) {
                return Ok(());
            }
        }
    }

    /// The server's sinks, cards and our own streams, read now.
    fn snapshot(&mut self, pid: u32) -> Result<Snapshot, protocol::ProtocolError> {
        let sinks = self.request::<protocol::SinkInfoList>(Command::GetSinkInfoList)?;
        let cards = self.request::<protocol::CardInfoList>(Command::GetCardInfoList)?;
        let clients = self.request::<protocol::ClientInfoList>(Command::GetClientInfoList)?;
        let inputs = self.request::<protocol::SinkInputInfoList>(Command::GetSinkInputInfoList)?;

        let mine: Vec<u32> = clients
            .iter()
            .filter(|client| prop_u32(&client.props, Prop::ApplicationProcessId) == Some(pid))
            .map(|client| client.index)
            .collect();
        let ours = inputs
            .iter()
            .filter(|input| {
                input
                    .client_index
                    .is_some_and(|client| mine.contains(&client))
            })
            .map(|input| StreamView {
                index: input.index,
                sink: input.sink_index,
            })
            .collect();
        let sinks = sinks
            .iter()
            .map(|sink| SinkView {
                index: sink.index,
                card: sink.card_index,
                active_port: sink
                    .ports
                    .get(sink.active_port)
                    .map(|port| text(&port.name)),
                ports: sink
                    .ports
                    .iter()
                    .map(|port| (text(&port.name), port_state(port.available)))
                    .collect(),
            })
            .collect();
        let cards = cards
            .iter()
            .map(|card| CardView {
                index: card.index,
                ports: card
                    .ports
                    .iter()
                    .map(|port| (text(&port.name), port_state(port.available)))
                    .collect(),
            })
            .collect();
        Ok(Snapshot { sinks, cards, ours })
    }
}

fn port_state(available: PortAvailable) -> PortState {
    match available {
        PortAvailable::Yes => PortState::Available,
        PortAvailable::No => PortState::Unavailable,
        PortAvailable::Unknown => PortState::Unknown,
    }
}

fn text(name: &CString) -> String {
    name.to_string_lossy().into_owned()
}

/// A numeric property. The protocol carries string values with their
/// terminating NUL.
fn prop_u32(props: &protocol::Props, prop: Prop) -> Option<u32> {
    let bytes = props.get(prop)?;
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    std::str::from_utf8(bytes).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_id_is_read_with_or_without_its_terminator() {
        let mut props = protocol::Props::new();
        props.set(Prop::ApplicationProcessId, c"1334048");
        assert_eq!(
            prop_u32(&props, Prop::ApplicationProcessId),
            Some(1_334_048)
        );
        props.set_bytes(Prop::ApplicationProcessId.to_c_str(), b"42");
        assert_eq!(prop_u32(&props, Prop::ApplicationProcessId), Some(42));
        props.set_bytes(Prop::ApplicationProcessId.to_c_str(), b"pid\0");
        assert_eq!(prop_u32(&props, Prop::ApplicationProcessId), None);
        assert_eq!(
            prop_u32(&protocol::Props::new(), Prop::ApplicationProcessId),
            None
        );
    }
}

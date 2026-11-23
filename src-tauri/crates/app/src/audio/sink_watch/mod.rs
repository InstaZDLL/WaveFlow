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
//! **The session manager may pause first.** WirePlumber 0.5 pauses a
//! media player itself, over MPRIS, when the sink its stream played on is
//! removed — measured: its `Pause` reached us before the verdict did. It
//! covers a sink that goes, not a jack pulled out of a built-in card,
//! where no sink does. When playback is already paused that recently, the
//! watch only posts the notice (see [`PAUSE_COINCIDENCE`]).
//!
//! **Fail-soft.** No server, a refused connection, a server that restarts:
//! the thread logs, waits and tries again, and the app plays exactly as it
//! did before this existed. Nothing here touches the audio callback or the
//! decoder.

pub mod decision;

use std::ffi::CString;
use std::io::{BufReader, Read};
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
/// every event until the server goes away. Returns `Ok` when an
/// established connection breaks, `Err` when none could be made.
fn watch(app: &AppHandle) -> Result<(), Fault> {
    let mut events = Connection::open()?;
    let mut queries = Connection::open()?;
    events.subscribe(
        SubscriptionMask::SINK | SubscriptionMask::SINK_INPUT | SubscriptionMask::CARD,
    )?;
    tracing::info!("sink watch: following the sound server's sinks");

    let pid = std::process::id();
    let mut tracker = Tracker::default();
    let mut said_unreadable = false;
    // The first picture is the baseline: nothing is judged against it.
    let mut next = queries.snapshot(pid, &tracker);
    loop {
        match next {
            Ok(snapshot) => {
                if let Some(loss) = tracker.observe(&snapshot) {
                    on_loss(app, loss);
                }
            }
            // A reply that did not decode. Its frame was read whole, so
            // the connection is still in step: skip this picture and
            // judge on the next, with what we knew kept as it was.
            Err(Fault::Reply(err)) if !said_unreadable => {
                tracing::info!(%err, "sink watch: could not read the sound server's answer");
                said_unreadable = true;
            }
            Err(Fault::Reply(err)) => {
                tracing::debug!(%err, "sink watch: could not read the sound server's answer");
            }
            Err(Fault::Broken(err)) => {
                tracing::info!(%err, "sink watch: the sound server stopped answering; reconnecting");
                return Ok(());
            }
        }
        // Only the three subscribed kinds arrive. One unplug sends a
        // burst of them, and each gets its own picture: a local query
        // costs little, and the removal and the move are often two
        // events apart.
        if let Err(err) = events.next_frame() {
            tracing::info!(%err, "sink watch: lost the sound server; reconnecting");
            return Ok(());
        }
        next = queries.snapshot(pid, &tracker);
    }
}

/// How close a pause made by someone else has to be to the loss to be
/// the loss's own. WirePlumber's MPRIS pause lands within milliseconds of
/// the sink's removal (measured); two seconds leaves room for a busy
/// machine without reaching back to a pause the user made.
const PAUSE_COINCIDENCE: Duration = Duration::from_secs(2);

/// The device under our stream went away and the server moved it.
///
/// WirePlumber 0.5 pauses media players itself when the sink their
/// stream played on is removed (`linking.pause-playback`, on by default),
/// and it finds us by our MPRIS name. Its pause and this verdict race:
/// measured on a removed sink, the pause had already landed when the
/// verdict came. Playback is then paused already, and all that is left
/// to do is say why. It does nothing for a jack pulled out of a built-in
/// card, where no sink is removed — the reported case — so there the
/// pause is ours.
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
    let shared = engine.shared();
    let playing = shared.state() == PlayerState::Playing || crate::player_actions::clip_is_open();
    if playing {
        tracing::info!(
            ?loss,
            "the output device went away and the sound server moved the stream; pausing playback"
        );
        crate::player_actions::pause(app);
    } else if shared.paused_within(PAUSE_COINCIDENCE) {
        tracing::info!(
            ?loss,
            "the output device went away and playback was paused at the same moment, \
             by the session manager; saying why"
        );
    } else {
        tracing::debug!(
            ?loss,
            "sink watch: the output device went away while nothing played"
        );
        return;
    }
    engine.emit_playback_notice(PlaybackNotice::PausedDeviceLost);
}

/// The PulseAudio protocol's own cap on a frame. Anything larger is not a
/// reply we asked for, and allocating it would be the bug.
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// What went wrong with a request.
#[derive(Debug)]
enum Fault {
    /// The connection cannot be used any more: no server, a closed
    /// socket, a frame we could not read whole.
    Broken(std::io::Error),
    /// The server answered, with an error or with something the client
    /// could not decode. The frame was read whole, so the connection is
    /// still in step.
    Reply(protocol::ProtocolError),
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Broken(err) => write!(f, "{err}"),
            Self::Reply(err) => write!(f, "{err}"),
        }
    }
}

impl From<std::io::Error> for Fault {
    fn from(err: std::io::Error) -> Self {
        Self::Broken(err)
    }
}

/// One connection to the server, request and reply in lockstep. Two are
/// opened because a subscribed connection interleaves events with
/// replies, and reading them apart is more code than a second socket.
///
/// Every frame is read whole before it is decoded. The client's readers
/// decode straight off the socket, and one that refuses a value — a
/// negative latency offset on a card port, which this version of the
/// client will not take — would leave the rest of the frame unread and
/// every later reply out of step.
struct Connection {
    socket: BufReader<UnixStream>,
    version: u16,
    seq: u32,
}

impl Connection {
    fn open() -> Result<Self, Fault> {
        let path = pulseaudio::socket_path_from_env().ok_or_else(|| {
            Fault::Broken(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no sound server socket",
            ))
        })?;
        let mut connection = Self {
            socket: BufReader::new(UnixStream::connect(path)?),
            version: protocol::MAX_VERSION,
            seq: 0,
        };
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
        let reply = connection.request::<protocol::AuthReply>(Command::Auth(auth))?;
        connection.version = protocol::MAX_VERSION.min(reply.version);
        let mut props = protocol::Props::new();
        props.set(Prop::ApplicationName, c"WaveFlow");
        connection.request::<protocol::SetClientNameReply>(Command::SetClientName(props))?;
        Ok(connection)
    }

    fn send(&mut self, command: &Command) -> Result<(), Fault> {
        self.seq = self.seq.wrapping_add(1);
        protocol::write_command_message(self.socket.get_mut(), self.seq, command, self.version)
            .map_err(|err| match err {
                protocol::ProtocolError::Io(err) => Fault::Broken(err),
                other => Fault::Reply(other),
            })
    }

    /// The next frame, descriptor included, read whole.
    fn next_frame(&mut self) -> std::io::Result<Vec<u8>> {
        let mut frame = vec![0; protocol::DESCRIPTOR_SIZE];
        self.socket.read_exact(&mut frame)?;
        let length = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
        if length > MAX_FRAME {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("a {length}-byte frame from the sound server"),
            ));
        }
        frame.resize(protocol::DESCRIPTOR_SIZE + length, 0);
        self.socket
            .read_exact(&mut frame[protocol::DESCRIPTOR_SIZE..])?;
        Ok(frame)
    }

    fn request<T: CommandReply>(&mut self, command: Command) -> Result<T, Fault> {
        self.send(&command)?;
        let frame = self.next_frame()?;
        let (_, reply) = protocol::read_reply_message::<T>(&mut frame.as_slice(), self.version)
            .map_err(Fault::Reply)?;
        Ok(reply)
    }

    fn subscribe(&mut self, mask: SubscriptionMask) -> Result<(), Fault> {
        self.send(&Command::Subscribe(mask))?;
        let frame = self.next_frame()?;
        protocol::read_ack_message(&mut frame.as_slice()).map_err(Fault::Reply)?;
        Ok(())
    }

    /// The server's sinks, our own streams, and the cards `tracker` is
    /// about to ask about, read now.
    ///
    /// Streams first: the server removes a sink before it moves the
    /// streams that were on it (measured), so reading the sinks first
    /// could see the old sink still listed and our stream already moved
    /// off it — a choice, by the rules — and the loss would never be
    /// judged again. Read in this order, a stream still on the old sink
    /// either finds it listed (nothing happened yet) or names a sink that
    /// is gone, which waits for the next picture.
    fn snapshot(&mut self, pid: u32, tracker: &Tracker) -> Result<Snapshot, Fault> {
        let inputs = self.request::<protocol::SinkInputInfoList>(Command::GetSinkInputInfoList)?;
        let clients = self.request::<protocol::ClientInfoList>(Command::GetClientInfoList)?;
        let sinks = self.request::<protocol::SinkInfoList>(Command::GetSinkInfoList)?;

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
        let mut snapshot = Snapshot {
            sinks,
            cards: Vec::new(),
            ours,
        };
        for index in tracker.cards_in_question(&snapshot) {
            let query = Command::GetCardInfo(protocol::GetCardInfo {
                index: Some(index),
                name: None,
            });
            match self.request::<protocol::CardInfo>(query) {
                Ok(card) => snapshot.cards.push(CardView {
                    index,
                    ports: card
                        .ports
                        .iter()
                        .map(|port| (text(&port.name), port_state(port.available)))
                        .collect(),
                }),
                // Gone: left out, which is what the decision reads.
                Err(Fault::Reply(protocol::ProtocolError::ServerError(
                    protocol::PulseError::NoEntity,
                ))) => {}
                // There, but unreadable: there with no ports, which never
                // stops the music.
                Err(Fault::Reply(err)) => {
                    tracing::debug!(%err, index, "sink watch: could not read a card");
                    snapshot.cards.push(CardView {
                        index,
                        ports: Vec::new(),
                    });
                }
                Err(broken) => return Err(broken),
            }
        }
        Ok(snapshot)
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

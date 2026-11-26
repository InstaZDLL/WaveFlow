//! The MPRIS object WaveFlow publishes on Linux, over zbus.
//!
//! souvlaki's Linux backends publish neither `Shuffle` nor `LoopStatus`,
//! so a desktop's media widget (KDE's, GNOME's extensions, `playerctl`)
//! greyed out both buttons for WaveFlow. Neither can be added from the
//! outside: the properties have to live on the same object, under the
//! same bus name, as the rest of `org.mpris.MediaPlayer2.Player`. So on
//! Linux the whole object is ours, and souvlaki stays the backend for
//! SMTC and MediaRemote only.
//!
//! Self-contained on purpose: nothing here knows about the queue, the
//! engine or Tauri. A call from the bus becomes a [`Request`] handed to
//! the caller's handler; what the player is doing comes in through the
//! `set_*` methods of [`Mpris`]. The mapping between the two vocabularies
//! is the parent module's.
//!
//! What the object answers is what souvlaki's answered, property for
//! property, plus the two it lacked:
//! - `Shuffle` and `LoopStatus` are read-write. A write is a *request*:
//!   the setter forwards it and leaves the published value alone, so the
//!   `PropertiesChanged` zbus sends right after a `Set` still carries the
//!   old value. The new one is published by [`Mpris::set_options`] once
//!   the player has saved it — a request the player refuses, or one that
//!   fails to save, leaves the widget showing the mode really in force.
//! - `PlaybackStatus` is re-announced on every [`Mpris::set_playback`],
//!   changed or not, as souvlaki did. The once-a-second position refresh
//!   goes through it, and that announcement is what tells a widget to
//!   re-read `Position` (which, by the specification, never signals).

use std::collections::HashMap;
use std::sync::Arc;

use zbus::blocking::connection::Builder;
use zbus::blocking::object_server::InterfaceRef;
use zbus::blocking::Connection;
use zbus::fdo::{self, RequestNameFlags};
use zbus::interface;
use zbus::names::WellKnownName;
use zbus::zvariant::{ObjectPath, Value};

/// The object path the specification fixes for every MPRIS player.
pub(super) const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";

/// `LoopStatus` as the specification spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LoopStatus {
    /// Playback stops when there are no more tracks to play.
    None,
    /// The current track starts again from the beginning once it ends.
    Track,
    /// The playback loops through a list of tracks.
    Playlist,
}

impl LoopStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Track => "Track",
            Self::Playlist => "Playlist",
        }
    }

    /// Exactly the three spellings of the specification; anything else
    /// is refused rather than read as `None`, so a client that sends a
    /// typo gets an error instead of turning repeat off.
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "None" => Some(Self::None),
            "Track" => Some(Self::Track),
            "Playlist" => Some(Self::Playlist),
            _ => None,
        }
    }
}

/// `PlaybackStatus` as the specification spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Status {
    Playing,
    Paused,
    Stopped,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Playing => "Playing",
            Self::Paused => "Paused",
            Self::Stopped => "Stopped",
        }
    }
}

/// What a client on the bus asked of the player.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Request {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    /// `Seek`: a relative move, in microseconds, negative for backwards.
    Seek(i64),
    /// `SetPosition`: an absolute position, in microseconds. Never
    /// negative and never past the track's length — those are dropped
    /// here, as the specification asks.
    SetPosition(i64),
    SetShuffle(bool),
    SetLoopStatus(LoopStatus),
}

/// The track as `Metadata` describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Metadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub art_url: Option<String>,
    /// `mpris:length`, in microseconds.
    pub length_us: Option<i64>,
}

/// Called for every [`Request`], on zbus's own executor thread.
pub(super) type Handler = Arc<dyn Fn(Request) + Send + Sync>;

/// A published MPRIS player. Dropping it closes the connection, which
/// releases the bus name.
pub(super) struct Mpris {
    connection: Connection,
}

impl Mpris {
    /// Publish the player on the session bus as
    /// `org.mpris.MediaPlayer2.<bus_suffix>`.
    pub(super) fn start(bus_suffix: &str, identity: &str, handler: Handler) -> zbus::Result<Self> {
        let mpris = Self::serve(Builder::session()?, identity, handler)?;
        // Requested after the build rather than through the builder, which
        // always adds `DoNotQueue`: like souvlaki, a second copy running
        // under another identifier waits in line for the name and takes it
        // when the first quits, instead of never appearing at all.
        let name = WellKnownName::try_from(format!("org.mpris.MediaPlayer2.{bus_suffix}"))?;
        mpris
            .connection
            .request_name_with_flags(name, RequestNameFlags::ReplaceExisting.into())?;
        Ok(mpris)
    }

    /// Serve the two interfaces on whatever connection `builder` makes —
    /// the session bus in the app, a peer-to-peer socket in the tests.
    fn serve(builder: Builder<'_>, identity: &str, handler: Handler) -> zbus::Result<Self> {
        let connection = builder
            .serve_at(
                OBJECT_PATH,
                Root {
                    identity: identity.to_string(),
                },
            )?
            .serve_at(OBJECT_PATH, Player::new(handler))?
            .build()?;
        Ok(Self { connection })
    }

    fn player(&self) -> zbus::Result<InterfaceRef<Player>> {
        self.connection
            .object_server()
            .interface::<_, Player>(OBJECT_PATH)
    }

    pub(super) fn set_metadata(&self, metadata: Metadata) -> zbus::Result<()> {
        let iface = self.player()?;
        let mut player = iface.get_mut();
        player.metadata = metadata;
        zbus::block_on(player.metadata_changed(iface.signal_emitter()))
    }

    /// The playback status and the position the player is at. Announced
    /// whether it changed or not — see the module documentation.
    pub(super) fn set_playback(&self, status: Status, position_us: i64) -> zbus::Result<()> {
        let iface = self.player()?;
        let mut player = iface.get_mut();
        player.status = status;
        player.position_us = position_us;
        zbus::block_on(player.playback_status_changed(iface.signal_emitter()))
    }

    /// The shuffle and repeat modes in force. Only a value that changed
    /// is announced: this runs on every options change the player makes,
    /// most of which touch one of the two.
    pub(super) fn set_options(&self, shuffle: bool, loop_status: LoopStatus) -> zbus::Result<()> {
        let iface = self.player()?;
        let mut player = iface.get_mut();
        let emitter = iface.signal_emitter();
        if player.shuffle != shuffle {
            player.shuffle = shuffle;
            zbus::block_on(player.shuffle_changed(emitter))?;
        }
        if player.loop_status != loop_status {
            player.loop_status = loop_status;
            zbus::block_on(player.loop_status_changed(emitter))?;
        }
        Ok(())
    }
}

/// `org.mpris.MediaPlayer2`.
struct Root {
    identity: String,
}

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    // Advertised and ignored, as souvlaki's object did: the player has no
    // handler for either yet.
    fn raise(&self) {}

    fn quit(&self) {}

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> &str {
        &self.identity
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

/// `org.mpris.MediaPlayer2.Player`.
struct Player {
    handler: Handler,
    status: Status,
    position_us: i64,
    metadata: Metadata,
    shuffle: bool,
    loop_status: LoopStatus,
}

impl Player {
    fn new(handler: Handler) -> Self {
        Self {
            handler,
            status: Status::Stopped,
            position_us: 0,
            metadata: Metadata::default(),
            shuffle: false,
            loop_status: LoopStatus::None,
        }
    }

    fn request(&self, request: Request) {
        (self.handler)(request);
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) {
        self.request(Request::Next);
    }

    fn previous(&self) {
        self.request(Request::Previous);
    }

    fn pause(&self) {
        self.request(Request::Pause);
    }

    fn play_pause(&self) {
        self.request(Request::PlayPause);
    }

    fn stop(&self) {
        self.request(Request::Stop);
    }

    fn play(&self) {
        self.request(Request::Play);
    }

    fn seek(&self, offset: i64) {
        self.request(Request::Seek(offset));
    }

    fn set_position(&self, _track_id: ObjectPath<'_>, position: i64) {
        // The specification: a negative position, or one past the end of
        // the track, is ignored.
        if position < 0 {
            return;
        }
        if matches!(self.metadata.length_us, Some(length) if position > length) {
            return;
        }
        self.request(Request::SetPosition(position));
    }

    fn open_uri(&self, _uri: String) -> fdo::Result<()> {
        // `SupportedUriSchemes` is empty: there is nothing a client may
        // ask us to open.
        Err(fdo::Error::NotSupported("WaveFlow opens no URI".into()))
    }

    #[zbus(property)]
    fn playback_status(&self) -> &str {
        self.status.as_str()
    }

    #[zbus(property)]
    fn loop_status(&self) -> &str {
        self.loop_status.as_str()
    }

    /// A request, not a write: see the module documentation.
    #[zbus(property)]
    fn set_loop_status(&mut self, value: String) -> fdo::Result<()> {
        let status = LoopStatus::parse(&value)
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("unknown LoopStatus {value:?}")))?;
        self.request(Request::SetLoopStatus(status));
        Ok(())
    }

    #[zbus(property)]
    fn shuffle(&self) -> bool {
        self.shuffle
    }

    /// A request, not a write: see the module documentation.
    #[zbus(property)]
    fn set_shuffle(&mut self, value: bool) {
        self.request(Request::SetShuffle(value));
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<&'static str, Value<'_>> {
        let mut dict = HashMap::new();
        // A constant id, as souvlaki's: `SetPosition` needs one to be
        // present, and its argument is not compared to anything.
        dict.insert(
            "mpris:trackid",
            Value::new(ObjectPath::from_static_str_unchecked("/")),
        );
        let metadata = &self.metadata;
        if let Some(length) = metadata.length_us {
            dict.insert("mpris:length", Value::new(length));
        }
        if let Some(url) = &metadata.art_url {
            dict.insert("mpris:artUrl", Value::new(url.as_str()));
        }
        if let Some(title) = &metadata.title {
            dict.insert("xesam:title", Value::new(title.as_str()));
        }
        if let Some(artist) = &metadata.artist {
            dict.insert("xesam:artist", Value::new(vec![artist.as_str()]));
        }
        if let Some(album) = &metadata.album {
            dict.insert("xesam:album", Value::new(album.as_str()));
        }
        dict
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        1.0
    }

    // Accepted and ignored, as souvlaki's object did: the specification
    // has the property writable whenever `CanControl` is true.
    #[zbus(property)]
    fn set_volume(&mut self, _value: f64) {}

    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        match self.status {
            Status::Playing | Status::Paused => self.position_us,
            Status::Stopped => 0,
        }
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc::{self, Receiver};
    use std::sync::Mutex;
    use std::time::Duration;

    use zbus::blocking::fdo::PropertiesProxy;
    use zbus::blocking::Proxy;
    use zbus::names::InterfaceName;
    use zbus::zvariant::OwnedValue;

    use super::*;

    const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

    /// The object, served over one end of a socket pair, and a client on
    /// the other end: the D-Bus protocol for real, without needing a bus.
    struct Peer {
        mpris: Mpris,
        client: Connection,
        requests: Receiver<Request>,
    }

    fn peer() -> Peer {
        let (server_end, client_end) = UnixStream::pair().unwrap();
        let (tx, requests) = mpsc::channel();
        let tx = Mutex::new(tx);
        let handler: Handler = Arc::new(move |request| {
            tx.lock().unwrap().send(request).unwrap();
        });
        // Both ends authenticate against each other, so the two builds
        // have to run at the same time.
        let server = std::thread::spawn(move || {
            #[allow(deprecated)]
            let builder = Builder::unix_stream(server_end)
                .server(zbus::Guid::generate())
                .unwrap()
                .p2p();
            Mpris::serve(builder, "WaveFlow", handler).unwrap()
        });
        #[allow(deprecated)]
        let client = Builder::unix_stream(client_end).p2p().build().unwrap();
        let mpris = server.join().unwrap();
        Peer {
            mpris,
            client,
            requests,
        }
    }

    fn player(client: &Connection) -> Proxy<'static> {
        zbus::blocking::proxy::Builder::new(client)
            .destination("org.mpris.MediaPlayer2.waveflow")
            .unwrap()
            .path(OBJECT_PATH)
            .unwrap()
            .interface(PLAYER)
            .unwrap()
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .unwrap()
    }

    /// Every `PropertiesChanged` the object sends, as the changed
    /// properties of each.
    fn changes(client: &Connection) -> Receiver<HashMap<String, OwnedValue>> {
        let properties = PropertiesProxy::builder(client)
            .destination("org.mpris.MediaPlayer2.waveflow")
            .unwrap()
            .path(OBJECT_PATH)
            .unwrap()
            .build()
            .unwrap();
        let stream = properties.receive_properties_changed().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for signal in stream {
                let args = signal.args().unwrap();
                assert_eq!(
                    args.interface_name(),
                    &InterfaceName::from_static_str_unchecked(PLAYER)
                );
                let changed = args
                    .changed_properties()
                    .iter()
                    .map(|(name, value)| (name.to_string(), value.try_to_owned().unwrap()))
                    .collect();
                if tx.send(changed).is_err() {
                    break;
                }
            }
        });
        rx
    }

    const WAIT: Duration = Duration::from_secs(5);
    const QUIET: Duration = Duration::from_millis(300);

    #[test]
    fn loop_status_spellings_round_trip_and_nothing_else_parses() {
        for status in [LoopStatus::None, LoopStatus::Track, LoopStatus::Playlist] {
            assert_eq!(LoopStatus::parse(status.as_str()), Some(status));
        }
        for wrong in ["", "none", "track", "All", "One", "Off"] {
            assert_eq!(LoopStatus::parse(wrong), None, "{wrong:?}");
        }
    }

    #[test]
    fn shuffle_and_loop_status_read_what_the_player_published() {
        let peer = peer();
        let player = player(&peer.client);
        assert!(!player.get_property::<bool>("Shuffle").unwrap());
        assert_eq!(player.get_property::<String>("LoopStatus").unwrap(), "None");

        peer.mpris.set_options(true, LoopStatus::Track).unwrap();
        assert!(player.get_property::<bool>("Shuffle").unwrap());
        assert_eq!(
            player.get_property::<String>("LoopStatus").unwrap(),
            "Track"
        );
    }

    #[test]
    fn a_write_is_forwarded_as_a_request_and_changes_nothing_by_itself() {
        let peer = peer();
        let player = player(&peer.client);

        player.set_property("Shuffle", true).unwrap();
        assert_eq!(
            peer.requests.recv_timeout(WAIT).unwrap(),
            Request::SetShuffle(true)
        );
        player.set_property("LoopStatus", "Playlist").unwrap();
        assert_eq!(
            peer.requests.recv_timeout(WAIT).unwrap(),
            Request::SetLoopStatus(LoopStatus::Playlist)
        );

        // Until the player confirms, the published values are the old ones.
        assert!(!player.get_property::<bool>("Shuffle").unwrap());
        assert_eq!(player.get_property::<String>("LoopStatus").unwrap(), "None");
    }

    #[test]
    fn an_unknown_loop_status_is_refused_and_not_forwarded() {
        let peer = peer();
        let player = player(&peer.client);
        let err = player.set_property("LoopStatus", "Forever").unwrap_err();
        assert!(matches!(err, fdo::Error::InvalidArgs(_)), "{err:?}");
        assert!(peer.requests.recv_timeout(QUIET).is_err());
    }

    #[test]
    fn a_change_made_in_the_player_is_announced_once_and_only_what_changed() {
        let peer = peer();
        let changes = changes(&peer.client);
        // The client's match is in place once a call has round-tripped.
        player(&peer.client)
            .get_property::<bool>("Shuffle")
            .unwrap();

        peer.mpris.set_options(true, LoopStatus::None).unwrap();
        let changed = changes.recv_timeout(WAIT).unwrap();
        assert_eq!(changed.keys().collect::<Vec<_>>(), ["Shuffle"]);
        assert!(bool::try_from(&changed["Shuffle"]).unwrap());

        peer.mpris.set_options(true, LoopStatus::Playlist).unwrap();
        let changed = changes.recv_timeout(WAIT).unwrap();
        assert_eq!(changed.keys().collect::<Vec<_>>(), ["LoopStatus"]);
        assert_eq!(
            <&str>::try_from(&changed["LoopStatus"]).unwrap(),
            "Playlist"
        );

        // The same state again announces nothing.
        peer.mpris.set_options(true, LoopStatus::Playlist).unwrap();
        assert!(changes.recv_timeout(QUIET).is_err());
    }

    #[test]
    fn the_rest_of_the_player_answers_as_before() {
        let peer = peer();
        let player = player(&peer.client);

        peer.mpris
            .set_metadata(Metadata {
                title: Some("Title".into()),
                artist: Some("Artist".into()),
                album: Some("Album".into()),
                art_url: Some("file:///cover.jpg".into()),
                length_us: Some(180_000_000),
            })
            .unwrap();
        peer.mpris
            .set_playback(Status::Playing, 42_000_000)
            .unwrap();
        assert_eq!(
            player.get_property::<String>("PlaybackStatus").unwrap(),
            "Playing"
        );
        assert_eq!(player.get_property::<i64>("Position").unwrap(), 42_000_000);
        let metadata: HashMap<String, OwnedValue> = player.get_property("Metadata").unwrap();
        assert_eq!(<&str>::try_from(&metadata["xesam:title"]).unwrap(), "Title");
        assert_eq!(
            i64::try_from(&metadata["mpris:length"]).unwrap(),
            180_000_000
        );

        player.call_method("PlayPause", &()).unwrap();
        assert_eq!(
            peer.requests.recv_timeout(WAIT).unwrap(),
            Request::PlayPause
        );
        player.call_method("Seek", &(-5_000_000i64)).unwrap();
        assert_eq!(
            peer.requests.recv_timeout(WAIT).unwrap(),
            Request::Seek(-5_000_000)
        );
        let track = ObjectPath::from_static_str_unchecked("/");
        player
            .call_method("SetPosition", &(&track, 10_000_000i64))
            .unwrap();
        assert_eq!(
            peer.requests.recv_timeout(WAIT).unwrap(),
            Request::SetPosition(10_000_000)
        );
        // Past the end of the track: ignored, as the specification asks.
        player
            .call_method("SetPosition", &(&track, 200_000_000i64))
            .unwrap();
        assert!(peer.requests.recv_timeout(QUIET).is_err());
    }
}

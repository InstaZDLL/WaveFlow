//! Joining a session: discovery, the connection, the replica, requests.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::time::Duration;

use futures::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tokio::sync::{mpsc, oneshot};
use waveflow_core::lan::address::PeerFilter;
use waveflow_core::lan::cert::Fingerprint;
use waveflow_core::lan::client;
use waveflow_core::lan::code::Code;
use waveflow_core::lan::mdns::{Browser, Event};
use waveflow_core::wavesync::wire::{decode_host, GuestMessage, HostMessage, Outcome};
use waveflow_core::wavesync::{Action, EntryId, ParticipantId, Refusal, Replica, PROTOCOL_VERSION};

use super::{
    display_name, join_error, next_text, refusal_error, refuse_offline, text, write_loop,
    EndReason, Session, Tx, WaveSync, ANSWER_TIMEOUT, CONTEXT, SERVICE,
};
use crate::error::{AppError, AppResult, ShownKind};

/// How long discovery listens before answering.
const DISCOVERY: Duration = Duration::from_millis(2_500);

/// Most sessions discovery lists: a LAN has a handful, and a network that
/// answers with more is not one to list in full.
const MAX_DISCOVERED: usize = 12;

pub(super) struct GuestSide {
    tx: Tx,
    /// `None` until the host has let this machine in.
    pub replica: Option<Replica>,
    pub me: Option<ParticipantId>,
    /// This library's tracks in the queue, by the entry that names them.
    pub own: HashMap<EntryId, i64>,
    /// Entries this machine asked to add and the host has not confirmed yet:
    /// kept in `own` while ops for other changes arrive before theirs.
    pub pending_own: HashSet<EntryId>,
    next_request: u32,
    waiting: HashMap<u32, oneshot::Sender<Outcome>>,
}

impl GuestSide {
    /// Say goodbye to the host; dropping the side closes the connection.
    pub fn leave(&self) {
        let _ = self.tx.send(text(&GuestMessage::Request {
            req: 0,
            action: Action::Leave,
        }));
    }

    /// Forget the own entries that left the queue, but not those still on
    /// their way into it.
    pub fn prune_own(&mut self) {
        let Some(replica) = self.replica.as_ref() else {
            return;
        };
        let state = replica.state();
        let pending = &self.pending_own;
        self.own
            .retain(|entry, _| pending.contains(entry) || state.index_of(*entry).is_some());
    }
}

/// A session another machine advertises.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovered {
    /// The session's name, as its host advertises it.
    pub name: String,
    /// The host's name.
    pub host: String,
    /// Where to join it.
    pub address: String,
    pub fingerprint: Option<String>,
}

impl WaveSync {
    /// The sessions advertised on the network, after listening a moment.
    pub async fn discover(&self) -> AppResult<Vec<Discovered>> {
        refuse_offline()?;
        let filter = PeerFilter::for_this_machine(false);
        // A host advertises every address it has, a VPN's or a virtual
        // adapter's included. The one to use is on this machine's own subnet,
        // and among several, on the subnet of its best interface: the order
        // DLNA ranks them in, a private physical network first.
        let local = crate::dlna::lan_addrs();
        let rank = |ip: std::net::IpAddr| match ip {
            std::net::IpAddr::V4(v4) => local
                .iter()
                .position(|l| l.contains(v4))
                .unwrap_or(usize::MAX),
            std::net::IpAddr::V6(_) => usize::MAX,
        };
        let own = match self.lock().session.as_ref() {
            Some(Session::Host(side)) => Some(side.fingerprint.to_string()),
            _ => None,
        };
        let mut browser = Browser::start(SERVICE).map_err(|e| {
            tracing::warn!(error = %e, "wavesync: discovery did not start");
            AppError::shown(
                ShownKind::WavesyncUnreachable,
                "Sessions cannot be looked for on this network.",
            )
        })?;
        let mut found: BTreeMap<String, Discovered> = BTreeMap::new();
        let _ = tokio::time::timeout(DISCOVERY, async {
            while let Some(event) = browser.next().await {
                match event {
                    Event::Found(service) => {
                        let Some(addr) = service
                            .addresses
                            .iter()
                            .filter(|a| filter.admits(a.ip()))
                            .min_by_key(|a| (rank(a.ip()), a.ip().is_loopback(), a.is_ipv6()))
                        else {
                            continue;
                        };
                        let fingerprint = service.txt.get("fp").cloned();
                        if fingerprint.is_some() && fingerprint == own {
                            continue;
                        }
                        if found.len() >= MAX_DISCOVERED && !found.contains_key(&service.fullname) {
                            continue;
                        }
                        let field = |key: &str| {
                            waveflow_core::wavesync::clean_name(
                                service.txt.get(key).map(String::as_str).unwrap_or_default(),
                            )
                        };
                        found.insert(
                            service.fullname.clone(),
                            Discovered {
                                name: field("sn"),
                                host: field("hn"),
                                address: addr.to_string(),
                                fingerprint,
                            },
                        );
                    }
                    Event::Lost { fullname } => {
                        found.remove(&fullname);
                    }
                }
            }
        })
        .await;
        Ok(found.into_values().collect())
    }

    /// Join the session at `target` (an address with its port, or an
    /// invitation link) with `code`, under `name`.
    pub async fn join(
        &self,
        app: &AppHandle,
        target: &str,
        fingerprint: Option<&str>,
        code: Option<&str>,
        name: &str,
    ) -> AppResult<super::View> {
        refuse_offline()?;
        let name = display_name(name)?;
        let asked = {
            let inner = self.lock();
            if inner.session.is_some() {
                return Err(already());
            }
            inner.generation
        };
        let target = parse_target(target)?;
        let pin = match fingerprint.filter(|f| !f.is_empty()) {
            Some(f) => Some(f.parse::<Fingerprint>().map_err(|_| invalid_address())?),
            None => target.fingerprint,
        };
        let code = code
            .filter(|c| !c.trim().is_empty())
            .or(target.code.as_deref())
            .and_then(Code::parse)
            .ok_or_else(|| {
                AppError::shown(
                    ShownKind::WavesyncInvalidCode,
                    "That is not a WaveSync code.",
                )
            })?;
        let filter = PeerFilter::for_this_machine(false);
        let joined = client::join(target.addr, pin, &code, CONTEXT, filter)
            .await
            .map_err(join_error)?;

        let (sink, stream) = joined.socket.split();
        let (tx, rx) = mpsc::unbounded_channel();
        let generation = {
            let mut inner = self.lock();
            if inner.session.is_some() {
                return Err(already());
            }
            // Offline mode may have come on during the exchange, or a profile
            // switch ended "the session" meanwhile: then the connection goes
            // with this scope.
            refuse_offline()?;
            if inner.generation != asked {
                return Err(no_session());
            }
            inner.generation += 1;
            inner.last_end = None;
            inner.session = Some(Session::Guest(Box::new(GuestSide {
                tx: tx.clone(),
                replica: None,
                me: None,
                own: HashMap::new(),
                pending_own: HashSet::new(),
                next_request: 1,
                waiting: HashMap::new(),
            })));
            inner.generation
        };
        tauri::async_runtime::spawn(write_loop(sink, rx));
        let _ = tx.send(text(&GuestMessage::Join {
            protocol: PROTOCOL_VERSION,
            name,
        }));
        tauri::async_runtime::spawn(read_loop(app.clone(), generation, stream));
        tracing::info!("wavesync: joined a host, waiting to be let in");
        self.emit(app);
        Ok(self.view())
    }

    /// Ask the host of session `generation` for `action` and wait for its
    /// answer. `Ok(false)` when no answer came in time: the host may still
    /// have applied it.
    pub(super) async fn request(&self, generation: u64, action: Action) -> AppResult<bool> {
        let (req, answer) = {
            let mut inner = self.lock();
            if inner.generation != generation {
                return Err(no_session());
            }
            let Some(Session::Guest(side)) = inner.session.as_mut() else {
                return Err(no_session());
            };
            if side.replica.is_none() {
                return Err(AppError::shown(
                    ShownKind::WavesyncNotAllowed,
                    "The host has not let this machine in yet.",
                ));
            }
            let req = side.next_request;
            side.next_request += 1;
            let (send, receive) = oneshot::channel();
            side.waiting.insert(req, send);
            let _ = side.tx.send(text(&GuestMessage::Request { req, action }));
            (req, receive)
        };
        match tokio::time::timeout(ANSWER_TIMEOUT, answer).await {
            Ok(Ok(Outcome::Applied { .. })) => Ok(true),
            Ok(Ok(Outcome::Refused { reason })) => Err(refusal_error(reason)),
            // The session ended under the request: its channel went with it.
            Ok(Err(_)) => Err(no_session()),
            Err(_) => {
                // Cleaned in the session that sent it: a newer one may have a
                // request under the same number.
                self.with_guest(generation, |side| side.waiting.remove(&req));
                Ok(false)
            }
        }
    }

    /// Whether the session running is still the one `generation` names, and
    /// a joined one; then `f` on it.
    pub(super) fn with_guest<R>(
        &self,
        generation: u64,
        f: impl FnOnce(&mut GuestSide) -> R,
    ) -> Option<R> {
        let mut inner = self.lock();
        if inner.generation != generation {
            return None;
        }
        match inner.session.as_mut() {
            Some(Session::Guest(side)) => Some(f(side)),
            _ => None,
        }
    }

    /// End the joined session `generation` for `reason`, if it still runs.
    fn guest_ended(&self, app: &AppHandle, generation: u64, reason: EndReason) {
        let ended = {
            let mut inner = self.lock();
            if inner.generation != generation || !matches!(inner.session, Some(Session::Guest(_))) {
                false
            } else {
                inner.session = None;
                inner.generation += 1;
                inner.last_end = Some(reason);
                true
            }
        };
        if ended {
            tracing::info!(?reason, "wavesync: the joined session ended");
            self.emit(app);
        }
    }
}

fn already() -> AppError {
    AppError::shown(
        ShownKind::WavesyncAlreadyInSession,
        "A WaveSync session is already running.",
    )
}

fn no_session() -> AppError {
    AppError::shown(
        ShownKind::WavesyncNoSession,
        "No WaveSync session is running.",
    )
}

fn invalid_address() -> AppError {
    AppError::shown(
        ShownKind::WavesyncInvalidAddress,
        "That is not an address with its port, nor a WaveSync invitation.",
    )
}

/// What to join: an address, and what an invitation adds to it.
#[derive(Debug, PartialEq)]
pub(super) struct Target {
    pub addr: SocketAddr,
    pub fingerprint: Option<Fingerprint>,
    pub code: Option<String>,
}

/// `192.168.1.20:43399`, `[fd00::2]:43399`, or an invitation
/// `wavesync://192.168.1.20:43399/?fp=<hex>&code=<CODE>`. Only an IP
/// literal with its port: a host name would be a DNS lookup nobody asked for.
/// Read by hand rather than through a URL parser, which would drop a port it
/// takes for its scheme's default.
pub(super) fn parse_target(target: &str) -> AppResult<Target> {
    let target = target.trim();
    let Some(rest) = target.strip_prefix("wavesync://") else {
        let addr = target
            .parse::<SocketAddr>()
            .map_err(|_| invalid_address())?;
        return Ok(Target {
            addr,
            fingerprint: None,
            code: None,
        });
    };
    let (authority, query) = match rest.split_once('?') {
        Some((authority, query)) => (authority, query),
        None => (rest, ""),
    };
    let addr = authority
        .trim_end_matches('/')
        .parse::<SocketAddr>()
        .map_err(|_| invalid_address())?;
    let mut fingerprint = None;
    let mut code = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "fp" => fingerprint = Some(value.parse().map_err(|_| invalid_address())?),
            "code" => code = Some(value.into_owned()),
            _ => {}
        }
    }
    Ok(Target {
        addr,
        fingerprint,
        code,
    })
}

/// The invitation for a host at `addr`.
pub(super) fn invitation(addr: SocketAddr, fingerprint: &Fingerprint, code: &Code) -> String {
    format!("wavesync://{addr}/?fp={fingerprint}&code={}", code.as_str())
}

async fn read_loop<S>(app: AppHandle, generation: u64, mut stream: S)
where
    S: futures::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    let ws = app.state::<WaveSync>();
    while let Some(frame) = next_text(&mut stream).await {
        let Ok(message) = decode_host(&frame) else {
            continue;
        };
        let next = ws.with_guest(generation, |side| on_message(side, message));
        match next {
            None => return,
            Some(Next::Changed) => ws.emit(&app),
            Some(Next::Quiet) => {}
            Some(Next::End(reason)) => {
                ws.guest_ended(&app, generation, reason);
                return;
            }
        }
    }
    ws.guest_ended(&app, generation, EndReason::HostLeft);
}

enum Next {
    Changed,
    Quiet,
    End(EndReason),
}

fn on_message(side: &mut GuestSide, message: HostMessage) -> Next {
    match message {
        HostMessage::Welcome {
            protocol,
            you,
            token: _,
            snapshot,
        } => {
            if protocol != PROTOCOL_VERSION {
                return Next::End(EndReason::Incompatible);
            }
            match Replica::from_snapshot(snapshot) {
                Ok(replica) => {
                    side.replica = Some(replica);
                    side.me = Some(you);
                    tracing::info!(participant = %you, "wavesync: let in");
                    Next::Changed
                }
                Err(_) => Next::End(EndReason::Diverged),
            }
        }
        HostMessage::Refused { reason } => Next::End(match reason {
            Refusal::ProtocolMismatch => EndReason::Incompatible,
            Refusal::SessionFull => EndReason::Full,
            Refusal::SessionEnded => EndReason::HostEnded,
            _ => EndReason::Refused,
        }),
        HostMessage::Ops { ops } => {
            let Some(replica) = side.replica.as_mut() else {
                return Next::Quiet;
            };
            for op in &ops {
                if replica.apply(op).is_err() {
                    return Next::End(EndReason::Diverged);
                }
            }
            let state = replica.state();
            if state.has_ended() {
                return Next::End(EndReason::HostEnded);
            }
            if side.me.is_some_and(|me| state.participant(me).is_none()) {
                return Next::End(EndReason::Removed);
            }
            side.prune_own();
            Next::Changed
        }
        HostMessage::Snapshot { snapshot } => match Replica::from_snapshot(snapshot) {
            Ok(replica) => {
                side.replica = Some(replica);
                side.prune_own();
                Next::Changed
            }
            Err(_) => Next::End(EndReason::Diverged),
        },
        HostMessage::Answer { req, outcome } => {
            if let Some(waiting) = side.waiting.remove(&req) {
                let _ = waiting.send(outcome);
            }
            Next::Quiet
        }
        HostMessage::Pong { .. } | HostMessage::Unsupported { .. } => Next::Quiet,
    }
}

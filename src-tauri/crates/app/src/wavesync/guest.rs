//! Joining a session: discovery, the connection, the replica, requests.

use std::collections::{BTreeMap, HashMap};
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
use waveflow_core::wavesync::{Action, EntryId, ParticipantId, Replica, PROTOCOL_VERSION};

use super::{
    display_name, join_error, next_text, refusal_error, refuse_offline, text, write_loop,
    EndReason, Session, Tx, WaveSync, ANSWER_TIMEOUT, CONTEXT, SERVICE,
};
use crate::error::{AppError, AppResult, ShownKind};

/// How long discovery listens before answering.
const DISCOVERY: Duration = Duration::from_millis(2_500);

pub(super) struct GuestSide {
    tx: Tx,
    /// `None` until the host has let this machine in.
    pub replica: Option<Replica>,
    pub me: Option<ParticipantId>,
    /// This library's tracks in the queue, by the entry that names them.
    pub own: HashMap<EntryId, i64>,
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
                        let Some(addr) = service.addresses.iter().find(|a| filter.admits(a.ip()))
                        else {
                            continue;
                        };
                        let fingerprint = service.txt.get("fp").cloned();
                        if fingerprint.is_some() && fingerprint == own {
                            continue;
                        }
                        let field = |key: &str| service.txt.get(key).cloned().unwrap_or_default();
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
        if self.lock().session.is_some() {
            return Err(AppError::shown(
                ShownKind::WavesyncAlreadyInSession,
                "A WaveSync session is already running.",
            ));
        }
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
                return Err(AppError::shown(
                    ShownKind::WavesyncAlreadyInSession,
                    "A WaveSync session is already running.",
                ));
            }
            inner.generation += 1;
            inner.last_end = None;
            inner.session = Some(Session::Guest(Box::new(GuestSide {
                tx: tx.clone(),
                replica: None,
                me: None,
                own: HashMap::new(),
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

    /// Ask the host for `action` and wait for its answer.
    pub(super) async fn request(&self, action: Action) -> AppResult<()> {
        let (req, answer) = {
            let mut inner = self.lock();
            let Some(Session::Guest(side)) = inner.session.as_mut() else {
                return Err(AppError::shown(
                    ShownKind::WavesyncNoSession,
                    "No WaveSync session is running.",
                ));
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
            Ok(Ok(Outcome::Applied { .. })) => Ok(()),
            Ok(Ok(Outcome::Refused { reason })) => Err(refusal_error(reason)),
            _ => {
                if let Some(Session::Guest(side)) = self.lock().session.as_mut() {
                    side.waiting.remove(&req);
                }
                Err(AppError::shown(
                    ShownKind::WavesyncNoAnswer,
                    "The host did not answer.",
                ))
            }
        }
    }

    /// Whether the session running is still the one `generation` names, and
    /// a joined one; then `f` on it.
    fn with_guest<R>(&self, generation: u64, f: impl FnOnce(&mut GuestSide) -> R) -> Option<R> {
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
/// literal: a host name would be a DNS lookup nobody asked for.
pub(super) fn parse_target(target: &str) -> AppResult<Target> {
    let target = target.trim();
    if let Some(rest) = target.strip_prefix("wavesync://") {
        let url = url::Url::parse(&format!("http://{rest}")).map_err(|_| invalid_address())?;
        let ip: std::net::IpAddr = match url.host() {
            Some(url::Host::Ipv4(v4)) => v4.into(),
            Some(url::Host::Ipv6(v6)) => v6.into(),
            _ => return Err(invalid_address()),
        };
        let port = url.port().ok_or_else(invalid_address)?;
        let mut fingerprint = None;
        let mut code = None;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "fp" => fingerprint = Some(value.parse().map_err(|_| invalid_address())?),
                "code" => code = Some(value.into_owned()),
                _ => {}
            }
        }
        return Ok(Target {
            addr: SocketAddr::new(ip, port),
            fingerprint,
            code,
        });
    }
    let addr = target
        .parse::<SocketAddr>()
        .map_err(|_| invalid_address())?;
    Ok(Target {
        addr,
        fingerprint: None,
        code: None,
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
                return Next::End(EndReason::Diverged);
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
        HostMessage::Refused { .. } => Next::End(EndReason::Refused),
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
            side.own.retain(|entry, _| state.index_of(*entry).is_some());
            Next::Changed
        }
        HostMessage::Snapshot { snapshot } => match Replica::from_snapshot(snapshot) {
            Ok(replica) => {
                side.replica = Some(replica);
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

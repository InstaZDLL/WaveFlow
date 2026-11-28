//! Hosting a session: the listener, the admission, the guests' requests.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::Instant;

use futures::StreamExt;
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;
use waveflow_core::lan::cert::{Fingerprint, Identity};
use waveflow_core::lan::code::Code;
use waveflow_core::lan::listener::{Accepted, Admission, Config, Incoming, Listener};
use waveflow_core::lan::mdns::Advertisement;
use waveflow_core::lan::token::{self, TokenHash};
use waveflow_core::lan::HANDSHAKE_TIMEOUT;
use waveflow_core::wavesync::wire::{
    check_protocol, decode_guest, GuestMessage, HostMessage, Outcome, WireError,
};
use waveflow_core::wavesync::{
    clean_name, Action, EntryId, Host, ParticipantId, Refusal, Secret, Sequenced, PROTOCOL_VERSION,
};

use super::{
    display_name, next_text, refusal_error, refuse_offline, text, write_loop, Session, Tx,
    WaveSync, CONTEXT, SERVICE,
};
use crate::error::{AppError, AppResult, ShownKind};

/// What a returning guest's token starts with.
const TOKEN_PREFIX: &str = "wst_";

pub(super) struct HostSide {
    pub host: Host,
    listener: Listener,
    _advertisement: Option<Advertisement>,
    pub fingerprint: Fingerprint,
    pub code: Code,
    pub approval: bool,
    pub conns: HashMap<u64, Conn>,
    next_conn: u64,
    next_request: u32,
    /// This library's tracks in the queue, by the entry that names them.
    pub own: HashMap<EntryId, i64>,
    pub wrong_codes: u32,
    started: Instant,
    pub started_at_ms: i64,
    pub ever_joined: bool,
    pub port: u16,
    pub address: Option<Ipv4Addr>,
}

pub(super) struct Conn {
    tx: Tx,
    pub state: ConnState,
}

pub(super) enum ConnState {
    /// Proved the code, sent its name, not admitted yet.
    Joining,
    /// Waiting for the host's approval.
    Pending { request: u32, name: String },
    Admitted {
        participant: ParticipantId,
        _token: TokenHash,
    },
}

impl HostSide {
    pub fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Send `ops` to every admitted guest but `except`.
    fn broadcast(&self, ops: &[Sequenced], except: Option<u64>) {
        if ops.is_empty() {
            return;
        }
        let message = text(&HostMessage::Ops { ops: ops.to_vec() });
        for (id, conn) in &self.conns {
            if Some(*id) != except && matches!(conn.state, ConnState::Admitted { .. }) {
                let _ = conn.tx.send(message.clone());
            }
        }
    }

    /// Apply the host's own `action`.
    pub fn act(&mut self, action: Action) -> Result<Vec<Sequenced>, Refusal> {
        let now = self.now_ms();
        let me = self.host.host_id();
        let ops = self.host.handle(me, action, now)?;
        self.broadcast(&ops, None);
        self.forget_removed();
        Ok(ops)
    }

    /// Drop the connections of participants the session no longer has, and
    /// the host's own entries that left the queue.
    fn forget_removed(&mut self) {
        let state = self.host.state();
        self.conns.retain(|_, conn| match conn.state {
            ConnState::Admitted { participant, .. } => state.participant(participant).is_some(),
            _ => true,
        });
        self.own.retain(|entry, _| state.index_of(*entry).is_some());
    }

    /// Tell every guest the session is over and let them go.
    pub fn end(&mut self) {
        let ops = self.host.end();
        self.broadcast(&ops, None);
        self.conns.clear();
    }

    /// Let the guest of `conn` in: its number, its token, the state.
    fn admit(&mut self, conn: u64, name: &str) {
        let admitted = self.host.admit(name);
        let Some(entry) = self.conns.get_mut(&conn) else {
            return;
        };
        match admitted {
            Ok((participant, ops)) => {
                let token = token::generate(TOKEN_PREFIX);
                entry.state = ConnState::Admitted {
                    participant,
                    _token: TokenHash::of(&token),
                };
                let _ = entry.tx.send(text(&HostMessage::Welcome {
                    protocol: PROTOCOL_VERSION,
                    you: participant,
                    token: Some(Secret::new(token)),
                    snapshot: self.host.snapshot(),
                }));
                self.broadcast(&ops, Some(conn));
                self.ever_joined = true;
                tracing::info!(%participant, "wavesync: a guest joined");
            }
            Err(reason) => {
                let _ = entry.tx.send(text(&HostMessage::Refused { reason }));
                self.conns.remove(&conn);
            }
        }
    }
}

impl WaveSync {
    /// Open a session hosted here.
    pub async fn start(
        &self,
        app: &AppHandle,
        name: &str,
        host_name: &str,
        approval: bool,
    ) -> AppResult<super::View> {
        refuse_offline()?;
        let host_name = display_name(host_name)?;
        let name = display_name(name)?;
        if self.lock().session.is_some() {
            return Err(AppError::shown(
                ShownKind::WavesyncAlreadyInSession,
                "A WaveSync session is already running.",
            ));
        }
        let start_failed = |e: &dyn std::fmt::Display| {
            tracing::warn!(error = %e, "wavesync: the listener did not start");
            AppError::shown(
                ShownKind::WavesyncStartFailed,
                "The session could not start on this network.",
            )
        };
        let identity = Identity::generate().map_err(|e| start_failed(&e))?;
        let (listener, incoming) = Listener::start(
            &identity,
            Config {
                context: CONTEXT.into(),
                allow_private_vpn: false,
                port: 0,
                handshake_timeout: HANDSHAKE_TIMEOUT,
            },
        )
        .await
        .map_err(|e| start_failed(&e))?;
        let host = Host::new(&name, &host_name, 0).map_err(super::refusal_error)?;
        let code = Code::generate();
        listener.set_code(Some(code.clone()));
        let port = listener.port();
        let fingerprint = identity.fingerprint();
        let fingerprint_hex = fingerprint.to_string();
        let host_shown = host.state().participants()[0].name.clone();
        let advertisement = Advertisement::start(
            SERVICE,
            port,
            &[
                ("v", "1"),
                ("fp", fingerprint_hex.as_str()),
                ("sn", host.state().name()),
                ("hn", host_shown.as_str()),
            ],
        )
        .map_err(|e| tracing::warn!(error = %e, "wavesync: not advertised"))
        .ok();
        let address = crate::dlna::lan_addrs().first().map(|a| a.ip);

        let generation = {
            let mut inner = self.lock();
            if inner.session.is_some() {
                return Err(AppError::shown(
                    ShownKind::WavesyncAlreadyInSession,
                    "A WaveSync session is already running.",
                ));
            }
            // Offline mode may have come on while the listener started: then
            // nothing is installed, and the listener and the advertisement go
            // with this scope.
            refuse_offline()?;
            inner.generation += 1;
            inner.last_end = None;
            inner.session = Some(Session::Host(Box::new(HostSide {
                host,
                listener,
                _advertisement: advertisement,
                fingerprint,
                code,
                approval,
                conns: HashMap::new(),
                next_conn: 0,
                next_request: 1,
                own: HashMap::new(),
                wrong_codes: 0,
                started: Instant::now(),
                started_at_ms: chrono::Utc::now().timestamp_millis(),
                ever_joined: false,
                port,
                address,
            })));
            inner.generation
        };
        tracing::info!(port, "wavesync: hosting");
        tauri::async_runtime::spawn(accept_loop(app.clone(), generation, incoming));
        self.emit(app);
        Ok(self.view())
    }

    /// Draw a new code; the old one opens nothing from now on.
    pub fn new_code(&self, app: &AppHandle) -> AppResult<()> {
        {
            let mut inner = self.lock();
            let Some(Session::Host(side)) = inner.session.as_mut() else {
                return Err(no_session());
            };
            side.code = Code::generate();
            side.listener.set_code(Some(side.code.clone()));
        }
        self.emit(app);
        Ok(())
    }

    /// Let in, or turn away, the guest asking under `request`.
    pub fn approve(&self, app: &AppHandle, request: u32, accept: bool) -> AppResult<()> {
        {
            let mut inner = self.lock();
            let Some(Session::Host(side)) = inner.session.as_mut() else {
                return Err(no_session());
            };
            let found = side.conns.iter().find_map(|(id, conn)| match &conn.state {
                ConnState::Pending { request: r, name } if *r == request => {
                    Some((*id, name.clone()))
                }
                _ => None,
            });
            let Some((conn, name)) = found else {
                return Ok(());
            };
            if accept {
                side.admit(conn, &name);
            } else if let Some(entry) = side.conns.remove(&conn) {
                let _ = entry.tx.send(text(&HostMessage::Refused {
                    reason: Refusal::NotAllowed,
                }));
            }
        }
        self.emit(app);
        Ok(())
    }

    /// Send a guest away for good.
    pub fn remove_participant(&self, app: &AppHandle, participant: u32) -> AppResult<()> {
        self.host_act(
            app,
            Action::RemoveParticipant {
                participant: ParticipantId(participant),
            },
        )
    }

    /// The host's own action, applied and broadcast.
    pub(super) fn host_act(&self, app: &AppHandle, action: Action) -> AppResult<()> {
        {
            let mut inner = self.lock();
            let Some(Session::Host(side)) = inner.session.as_mut() else {
                return Err(no_session());
            };
            side.act(action).map_err(refusal_error)?;
        }
        self.emit(app);
        Ok(())
    }

    /// Whether the session running is still the one `generation` names, and
    /// a hosted one; then `f` on it.
    fn with_host<R>(&self, generation: u64, f: impl FnOnce(&mut HostSide) -> R) -> Option<R> {
        let mut inner = self.lock();
        if inner.generation != generation {
            return None;
        }
        match inner.session.as_mut() {
            Some(Session::Host(side)) => Some(f(side)),
            _ => None,
        }
    }
}

fn no_session() -> AppError {
    AppError::shown(
        ShownKind::WavesyncNoSession,
        "No WaveSync session is running.",
    )
}

async fn accept_loop(app: AppHandle, generation: u64, mut incoming: mpsc::Receiver<Incoming>) {
    while let Some(event) = incoming.recv().await {
        let ws = app.state::<WaveSync>();
        match event {
            Incoming::WrongCode { total } => {
                if ws
                    .with_host(generation, |side| side.wrong_codes = total)
                    .is_none()
                {
                    return;
                }
                ws.emit(&app);
            }
            Incoming::Accepted(accepted) => {
                let accepted = *accepted;
                if accepted.admission != Admission::Code {
                    // A returning guest's token: reconnection comes in lot 5.
                    continue;
                }
                tauri::async_runtime::spawn(serve(app.clone(), generation, accepted));
            }
        }
    }
}

/// One guest's connection, from its first message to its last.
async fn serve(app: AppHandle, generation: u64, accepted: Accepted) {
    let (sink, mut stream) = accepted.socket.split();
    let (tx, rx) = mpsc::unbounded_channel();
    tauri::async_runtime::spawn(write_loop(sink, rx));
    let ws = app.state::<WaveSync>();

    let Some(first) = next_text(&mut stream).await else {
        return;
    };
    let name = match decode_guest(&first) {
        Ok(GuestMessage::Join { protocol, name }) => match check_protocol(protocol) {
            // Cleaned here, as the session will keep it, so that the name the
            // host approves is the name it admits.
            Ok(()) => clean_name(&name),
            Err(reason) => {
                let _ = tx.send(text(&HostMessage::Refused { reason }));
                return;
            }
        },
        _ => {
            let _ = tx.send(text(&HostMessage::Refused {
                reason: Refusal::NotAllowed,
            }));
            return;
        }
    };
    if name.is_empty() {
        let _ = tx.send(text(&HostMessage::Refused {
            reason: Refusal::InvalidName,
        }));
        return;
    }

    let Some(conn) = ws.with_host(generation, |side| {
        let conn = side.next_conn;
        side.next_conn += 1;
        side.conns.insert(
            conn,
            Conn {
                tx: tx.clone(),
                state: ConnState::Joining,
            },
        );
        if side.approval {
            let request = side.next_request;
            side.next_request += 1;
            if let Some(entry) = side.conns.get_mut(&conn) {
                entry.state = ConnState::Pending { request, name };
            }
        } else {
            side.admit(conn, &name);
        }
        conn
    }) else {
        return;
    };
    drop(tx);
    ws.emit(&app);

    while let Some(frame) = next_text(&mut stream).await {
        let handled = ws.with_host(generation, |side| on_message(side, conn, &frame));
        match handled {
            None | Some(Flow::Close) => break,
            Some(Flow::Changed) => ws.emit(&app),
            Some(Flow::Quiet) => {}
        }
    }

    let gone = ws.with_host(generation, |side| {
        let Some(entry) = side.conns.remove(&conn) else {
            return true;
        };
        if let ConnState::Admitted { participant, .. } = entry.state {
            let ops = side.host.connection_lost(participant);
            side.broadcast(&ops, None);
            tracing::info!(%participant, "wavesync: a guest's connection dropped");
        }
        true
    });
    if gone.is_some() {
        ws.emit(&app);
    }
}

enum Flow {
    Changed,
    Quiet,
    Close,
}

/// A text frame from the guest of `conn`.
fn on_message(side: &mut HostSide, conn: u64, frame: &str) -> Flow {
    let Some(entry) = side.conns.get(&conn) else {
        return Flow::Close;
    };
    let tx = entry.tx.clone();
    let participant = match entry.state {
        ConnState::Admitted { participant, .. } => Some(participant),
        _ => None,
    };
    match decode_guest(frame) {
        Ok(GuestMessage::Ping { sent_ms }) => {
            let _ = tx.send(text(&HostMessage::Pong {
                sent_ms,
                host_ms: side.now_ms(),
            }));
            Flow::Quiet
        }
        Ok(GuestMessage::Request { req, action }) => {
            let Some(participant) = participant else {
                let _ = tx.send(text(&HostMessage::Answer {
                    req,
                    outcome: Outcome::Refused {
                        reason: Refusal::NotAllowed,
                    },
                }));
                return Flow::Quiet;
            };
            let leaving = matches!(action, Action::Leave);
            let now = side.now_ms();
            let outcome = match side.host.handle(participant, action, now) {
                Ok(ops) => {
                    side.broadcast(&ops, None);
                    side.forget_removed();
                    Outcome::Applied {
                        seq: side.host.seq(),
                    }
                }
                Err(reason) => Outcome::Refused { reason },
            };
            let _ = tx.send(text(&HostMessage::Answer { req, outcome }));
            if leaving {
                side.conns.remove(&conn);
                tracing::info!(%participant, "wavesync: a guest left");
                return Flow::Close;
            }
            Flow::Changed
        }
        Ok(GuestMessage::Join { .. } | GuestMessage::Resume { .. }) => {
            let _ = tx.send(text(&HostMessage::Refused {
                reason: Refusal::NotAllowed,
            }));
            Flow::Close
        }
        Err(WireError::Unsupported(kind)) => {
            let _ = tx.send(text(&HostMessage::Unsupported { kind }));
            Flow::Quiet
        }
        Err(_) => Flow::Close,
    }
}

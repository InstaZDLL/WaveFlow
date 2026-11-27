//! What the interface draws, built from the state in one place.

use std::collections::HashMap;
use std::net::SocketAddr;

use serde::Serialize;
use waveflow_core::wavesync::{EntryId, ParticipantState, Role, SessionState};

use super::host::ConnState;
use super::{EndReason, Inner, Session};

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub status: Status,
    /// `host` or `guest` while a session runs.
    pub role: Option<&'static str>,
    pub ended_reason: Option<EndReason>,
    pub session_name: Option<String>,
    pub me: Option<u32>,
    /// The host's: the code to say aloud, as `ABCD-EFGH`.
    pub code: Option<String>,
    /// The host's: its certificate in four groups, to compare.
    pub fingerprint: Option<String>,
    /// The host's: the link to send to someone who cannot see the session.
    pub invitation: Option<String>,
    pub participants: Vec<ParticipantView>,
    pub queue: Vec<EntryView>,
    /// The host's: who is waiting to be let in.
    pub pending: Vec<PendingView>,
    pub approval: bool,
    pub wrong_codes: u32,
    /// The host's: when the session started (ms since the epoch) and whether
    /// anybody ever joined, for the firewall hint after a minute alone.
    pub started_at_ms: Option<i64>,
    pub ever_joined: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    #[default]
    Idle,
    /// Connected, waiting for the host to let this machine in.
    Waiting,
    Active,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipantView {
    pub id: u32,
    pub name: String,
    pub host: bool,
    pub me: bool,
    pub state: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryView {
    pub id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub duration_ms: u64,
    pub owner_id: u32,
    pub owner_name: Option<String>,
    pub mine: bool,
    /// The library track, for this machine's own entries only.
    pub track_id: Option<i64>,
    /// `played`, `current`, `upcoming` or `ownerAway`.
    pub state: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingView {
    pub request: u32,
    pub name: String,
}

pub(super) fn build(inner: &Inner) -> View {
    let mut view = View {
        ended_reason: inner.last_end,
        ..View::default()
    };
    match &inner.session {
        None => {}
        Some(Session::Host(side)) => {
            view.status = Status::Active;
            view.role = Some("host");
            fill(
                &mut view,
                side.host.state(),
                Some(side.host.host_id().0),
                &side.own,
            );
            view.code = Some(side.code.grouped());
            view.fingerprint = Some(side.fingerprint.short());
            view.invitation = side.address.map(|ip| {
                super::guest::invitation(
                    SocketAddr::from((ip, side.port)),
                    &side.fingerprint,
                    &side.code,
                )
            });
            let mut pending: Vec<PendingView> = side
                .conns
                .values()
                .filter_map(|conn| match &conn.state {
                    ConnState::Pending { request, name } => Some(PendingView {
                        request: *request,
                        name: name.clone(),
                    }),
                    _ => None,
                })
                .collect();
            pending.sort_by_key(|p| p.request);
            view.pending = pending;
            view.approval = side.approval;
            view.wrong_codes = side.wrong_codes;
            view.started_at_ms = Some(side.started_at_ms);
            view.ever_joined = side.ever_joined;
        }
        Some(Session::Guest(side)) => {
            view.role = Some("guest");
            match &side.replica {
                None => view.status = Status::Waiting,
                Some(replica) => {
                    view.status = Status::Active;
                    fill(&mut view, replica.state(), side.me.map(|p| p.0), &side.own);
                }
            }
        }
    }
    view
}

fn fill(view: &mut View, state: &SessionState, me: Option<u32>, own: &HashMap<EntryId, i64>) {
    view.session_name = Some(state.name().to_string());
    view.me = me;
    view.participants = state
        .participants()
        .iter()
        .map(|p| ParticipantView {
            id: p.id.0,
            name: p.name.clone(),
            host: p.role == Role::Host,
            me: Some(p.id.0) == me,
            state: match p.state {
                ParticipantState::Listening => "listening",
                ParticipantState::Buffering => "buffering",
                ParticipantState::NotFollowing => "notFollowing",
                ParticipantState::Away => "away",
            },
        })
        .collect();
    let current = state.current().map(|e| e.id);
    view.queue = state
        .queue()
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let owner = state.participant(entry.owner);
            let away = !owner.is_some_and(|o| o.state != ParticipantState::Away);
            EntryView {
                id: entry.id.to_string(),
                title: entry.title.clone(),
                artists: entry.artists.clone(),
                album: entry.album.clone(),
                duration_ms: entry.duration_ms,
                owner_id: entry.owner.0,
                owner_name: owner.map(|o| o.name.clone()),
                mine: Some(entry.owner.0) == me,
                track_id: own.get(&entry.id).copied(),
                state: if index < state.cursor() {
                    "played"
                } else if Some(entry.id) == current {
                    "current"
                } else if away {
                    "ownerAway"
                } else {
                    "upcoming"
                },
            }
        })
        .collect();
}

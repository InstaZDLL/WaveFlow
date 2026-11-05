//! An episode of a bound server's channel, playing.
//!
//! The same session as a local episode ([`super::playback`]), so one detached queue
//! at a time and every surface that steps it holds without a copy of its rules. What
//! differs is where things are kept. Nothing here reads or writes a podcast row or a
//! `play_event`: the position is the server's, through its bookmark, and the server
//! keeps no played state, so an episode heard to its end loses its bookmark and opens
//! from the start, as the server's own web client has it.
//!
//! ## Requests come first
//!
//! A refusal changes nothing: what plays keeps playing. So [`play_episode`] and
//! [`jump_to`] read before they take the publish lock, and a step, which has to move the
//! cursor under that lock to know where it lands, reads the episode after letting it go. Either takes the lock again to
//! publish, and only if nothing newer got there first: a step that fails after being
//! overtaken stops nothing. Holding the lock across a request would park every other
//! surface for the length of a network call.
//!
//! ## The ticket
//!
//! Not in the load, which is printed and kept for a rebuild, and which a ticket would
//! outlive: it is good for about an hour, which an episode easily is. The decoder asks
//! for one when it opens the stream, and again when a request answers that it does not
//! know the one it has ([`super::stream`]). Over the enclosure's reader, so it is in
//! no log line, redacted or not, and every request is asked about the switch, offline
//! mode and this server still being the one the profile is bound to.
//!
//! The original bytes, whatever the transcode preference says: a transcode cannot
//! seek, and resuming is a seek.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use url::Url;
use waveflow_core::podcast::address::parse_address;
use waveflow_core::podcast::FetchFailure;

use super::fetch::Fetcher;
use super::playback::{
    announce, display_title, is_server_session, notify_session_started, EpisodeEntry, EpisodeHome,
    EpisodeLoad, EpisodeSource, PodcastPlayback, Step,
};
use super::refresh::PodcastError;
use super::store;
use super::stream::{EnclosureSource, Gate, Renew, StreamRefusal};
use crate::audio::engine::{AudioCmd, LoadIntent};
use crate::audio::AudioEngine;
use crate::error::{AppError, AppResult};
use crate::queue::{Direction, RepeatMode};
use crate::remote::client::RemoteClient;
use crate::remote::podcasts::{self, failure_word, ServerEpisode, ServerScope};
use crate::remote::stream::mint_ticket;
use crate::remote_playback::DetachedQueue;
use crate::state::AppState;

/// Emitted when a server's episode or channel has probably changed from here: a
/// position written at a pause, a seek or an end. Carries `channel_id`.
pub const PODCAST_SERVER_UPDATED: &str = "podcast:server-updated";

/// The word for an episode the server holds no file of.
const NOT_STORED: &str = "not_stored";

/// The word for a server that did not answer.
const UNREACHABLE: &str = "server_unreachable";

/// The word for a server that no longer takes this account's token.
const SIGNED_OUT: &str = "server_signed_out";

/// Where a server's episode is, for the session and the decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEpisodeRef {
    pub scope: ServerScope,
    pub episode_id: String,
    pub channel_id: String,
}

/// All of what writing one position may take: its turn, a token refresh, the request.
const POSITION_BUDGET: Duration = Duration::from_secs(8);

/// One write at a time, in the order they were asked for: a tick still on its way
/// must not land after the end that cleared the bookmark.
pub(crate) static POSITION_WRITES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The speed of a server's channel. The slider has no row of the channel to write to,
/// so this lasts as long as the app does: a voice keeps it through the queue, and it
/// never reaches `audio.playback_speed`.
static SPEEDS: LazyLock<Mutex<HashMap<(i64, String), f32>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn podcast_refusal(word: &str) -> AppError {
    AppError::Other(format!("podcast: {word}"))
}

/// Whether a request may leave for the server: the same two questions as for any
/// podcast, the switch first. The traffic goes to a host this person already chose, and
/// the rule stays one rule: off means no request.
pub(crate) async fn may_ask(pool: &sqlx::SqlitePool, offline: bool) -> Result<(), PodcastError> {
    if !store::is_enabled(pool).await? {
        return Err(PodcastError::Disabled);
    }
    if offline {
        return Err(PodcastError::Offline);
    }
    Ok(())
}

/// A client for the active profile's server, once the switch and offline mode have
/// had their say. Nothing is contacted before both have, a token refresh included.
pub(crate) async fn ready_client(state: &AppState) -> AppResult<Option<(RemoteClient<'_>, i64)>> {
    let profile_id = {
        let (pool, profile_id) = state.require_profile_snapshot().await?;
        may_ask(&pool, crate::offline::is_offline()).await?;
        profile_id
        // The lease ends here: holding it across a request would stall a switch.
    };
    match RemoteClient::try_build_for(state, profile_id).await {
        Ok(client) => Ok(client.map(|client| (client, profile_id))),
        // A token refresh that failed. Its text is the client's own, and what the page
        // wants is a word.
        Err(
            err @ (AppError::RemoteRefreshRejected(_)
            | AppError::RemoteRefreshUnanswered(_)
            | AppError::Other(_)),
        ) => {
            // A refresh takes its time, and the profile it was for may be gone by now.
            let stands = state
                .require_profile_pool_for(Some(profile_id))
                .await
                .is_ok();
            Err(why_no_client(&err, stands).refusal())
        }
        Err(err) => Err(err),
    }
}

/// Runs `work` so that whoever waits for it can give up without stopping it.
///
/// For anything that builds a client while somebody may stop waiting. A client may have
/// to refresh its token, and the server spends the old one the moment it is asked: a
/// future dropped before the answer is stored loses the new pair too, and leaves the
/// profile signed out with a valid session on the server.
pub(crate) async fn detached<T: Send + 'static>(
    work: impl Future<Output = T> + Send + 'static,
) -> Option<T> {
    tauri::async_runtime::spawn(work).await.ok()
}

/// Why there is no client for a scope. Not the same thing to the person listening: one
/// says the episode is no longer theirs to play, the other that the server did not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoClient {
    /// Another profile, another server, another account, signed out.
    ScopeGone,
    /// The scope stands, and the server refused the account's token.
    SignedOut,
    /// The scope stands, and a token refresh failed some other way: the network, most
    /// of the time.
    Unreachable,
}

/// Why building a client failed. A sign-out or a profile switch while the refresh queued
/// leaves the scope gone, whatever the error says, and the server says the rest: its 401
/// to a refresh is the one answer that means the session is over.
pub(crate) fn why_no_client(err: &AppError, scope_stands: bool) -> NoClient {
    match err {
        _ if !scope_stands => NoClient::ScopeGone,
        AppError::RemoteRefreshRejected(401) => NoClient::SignedOut,
        _ => NoClient::Unreachable,
    }
}

impl NoClient {
    pub(crate) fn refusal(self) -> AppError {
        match self {
            Self::ScopeGone => PodcastError::ProfileChanged.into(),
            Self::SignedOut => podcast_refusal(SIGNED_OUT),
            Self::Unreachable => podcast_refusal(UNREACHABLE),
        }
    }

    pub(crate) fn stream_refusal(self) -> StreamRefusal {
        match self {
            // Nothing here is for whoever is bound now, so nothing to say to them.
            Self::ScopeGone => StreamRefusal::Released,
            Self::SignedOut => StreamRefusal::SignedOut,
            Self::Unreachable => StreamRefusal::Failed(FetchFailure::Unreached),
        }
    }
}

/// Whether the rows say `scope` still stands. No request.
async fn scope_stands(state: &AppState, scope: &ServerScope) -> bool {
    let Ok(pool) = state.require_profile_pool_for(Some(scope.profile_id)).await else {
        return false;
    };
    let Ok(mut conn) = pool.acquire().await else {
        return false;
    };
    scope.is_current(&mut conn).await.unwrap_or(false)
}

/// A client still speaking for `scope`. Building one may refresh its token, so a failure
/// is told apart by what the rows say afterwards and what the server answered.
async fn client_in_scope<'a>(
    state: &'a AppState,
    scope: &ServerScope,
) -> Result<RemoteClient<'a>, NoClient> {
    match RemoteClient::try_build_for(state, scope.profile_id).await {
        Ok(Some(client)) if scope.holds_for(&client) => Ok(client),
        Ok(_) => Err(NoClient::ScopeGone),
        Err(err) => Err(why_no_client(&err, scope_stands(state, scope).await)),
    }
}

fn entry_of(episode: &ServerEpisode, scope: &ServerScope) -> EpisodeEntry {
    EpisodeEntry {
        home: EpisodeHome::Server(Box::new(ServerEpisodeRef {
            scope: scope.clone(),
            episode_id: episode.id.clone(),
            channel_id: episode.channel_id.clone(),
        })),
        profile_id: scope.profile_id,
        title: display_title(&episode.title, &episode.channel_title),
        channel: episode.channel_title.clone(),
        duration_ms: episode.duration_ms,
    }
}

/// The queue a channel's episodes form, newest first as the server lists them: what
/// is stored, since nothing else plays. A word when `current` is not in it, or not
/// stored.
pub(crate) fn queue_of(
    episodes: &[ServerEpisode],
    current: &str,
    scope: &ServerScope,
) -> Result<(Vec<EpisodeEntry>, usize), &'static str> {
    let wanted = episodes
        .iter()
        .find(|episode| episode.id == current)
        .ok_or("episode_gone")?;
    if !wanted.is_stored() {
        return Err(NOT_STORED);
    }
    let stored: Vec<&ServerEpisode> = episodes.iter().filter(|e| e.is_stored()).collect();
    let index = stored
        .iter()
        .position(|episode| episode.id == current)
        .unwrap_or(0);
    Ok((
        stored
            .into_iter()
            .map(|episode| entry_of(episode, scope))
            .collect(),
        index,
    ))
}

/// The word for a read of one episode that failed: the server knows nothing of it,
/// or something else went wrong.
fn read_refusal(failure: &crate::remote::client::RemoteFailure) -> AppError {
    match failure.status {
        Some(404) => PodcastError::EpisodeGone.into(),
        _ => podcast_refusal(&failure_word(failure)),
    }
}

/// The cover, in the cache of this profile's server covers. Asks the server the first
/// time and not after. A cover nobody could fetch is a player with no picture.
async fn artwork_path(state: &AppState, profile_id: i64, hash: Option<&str>) -> Option<String> {
    crate::remote::artwork::cached_path_for(state, profile_id, hash?)
        .await
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

fn load_of(
    episode: &ServerEpisode,
    scope: &ServerScope,
    artwork_path: Option<String>,
) -> EpisodeLoad {
    let duration_ms = episode.duration_ms.unwrap_or(0).max(0) as u64;
    let channel_id = episode.channel_id.clone();
    let speed = SPEEDS
        .lock()
        .ok()
        .and_then(|speeds| speeds.get(&(scope.profile_id, channel_id)).copied())
        .unwrap_or(1.0);
    EpisodeLoad {
        home: EpisodeHome::Server(Box::new(ServerEpisodeRef {
            scope: scope.clone(),
            episode_id: episode.id.clone(),
            channel_id: episode.channel_id.clone(),
        })),
        profile_id: scope.profile_id,
        track_id: crate::commands::player::next_radio_track_id(),
        start_ms: crate::queue::resume_position(
            episode.position_ms.unwrap_or(0).max(0) as u64,
            duration_ms,
        ),
        duration_ms,
        title: display_title(&episode.title, &episode.channel_title),
        channel: episode.channel_title.clone(),
        artwork_path,
        speed,
        source: EpisodeSource::Server {
            declared_type: episode.content_type.clone(),
        },
    }
}

/// Starts an episode, with its channel's stored episodes as the queue. A refusal
/// changes nothing: `not_stored` for one the server holds no file of.
pub(crate) async fn play_episode(
    app: &AppHandle,
    channel_id: &str,
    episode_id: &str,
) -> AppResult<()> {
    let engine = app.state::<Arc<AudioEngine>>();
    // Claimed before anything is read (#622): a track picked while this prepares is
    // the newer choice.
    let intent = engine.next_load_intent();
    let state = app.state::<AppState>();
    let (client, profile_id) = ready_client(&state)
        .await?
        .ok_or_else(|| podcast_refusal(SIGNED_OUT))?;
    let scope = ServerScope::of(&client, profile_id).ok_or_else(|| podcast_refusal(SIGNED_OUT))?;
    let channel = podcasts::channel(&client, channel_id)
        .await
        .map_err(|err| {
            tracing::debug!(status = ?err.status, "podcast: a server channel could not be read");
            read_refusal(&err)
        })?;
    let episodes = channel.episodes.unwrap_or_default();
    let (entries, index) = queue_of(&episodes, episode_id, &scope).map_err(podcast_refusal)?;
    let episode = episodes
        .iter()
        .find(|episode| episode.id == episode_id)
        .ok_or(PodcastError::EpisodeGone)?;
    let artwork = artwork_path(&state, profile_id, episode.artwork_hash.as_deref()).await;
    let load = load_of(episode, &scope, artwork);

    // Everything from here on is local.
    let _publish = engine.lock_publish().await;
    // A profile switch clears a session and claims no intent, so nothing else would
    // stop this one's queue from being installed in the next profile.
    state
        .require_profile_pool_for(Some(profile_id))
        .await
        .map_err(|_| PodcastError::ProfileChanged)?;
    if !engine.claim_dispatch(intent) {
        tracing::debug!("server episode superseded before it started; not installing it");
        return Ok(());
    }
    // One detached queue at a time.
    state.remote_playback.clear();
    state.external_playback.clear();
    let revision = state.podcast_playback.set(DetachedQueue { entries, index });
    notify_session_started();
    announce(app, &load);
    let played = engine.send(AudioCmd::LoadEpisodeAndPlay {
        intent,
        episode: Box::new(load),
    });
    if played.is_err() {
        let _ = engine.send(AudioCmd::Stop);
        state.podcast_playback.clear_if(revision);
    } else if engine.load_intent_superseded(intent) {
        state.podcast_playback.clear_if(revision);
    }
    played
}

/// What the player needs of a server's episode, read now: the row, and its cover. Moves
/// nothing, so whoever calls this can still refuse with nothing to undo. Refused for an
/// episode the server no longer holds a file of.
async fn read_for_player(
    state: &AppState,
    place: &ServerEpisodeRef,
) -> AppResult<(ServerEpisode, Option<String>)> {
    {
        let pool = state
            .require_profile_pool_for(Some(place.scope.profile_id))
            .await
            .map_err(|_| PodcastError::ProfileChanged)?;
        may_ask(&pool, crate::offline::is_offline()).await?;
    }
    let client = client_in_scope(state, &place.scope)
        .await
        .map_err(NoClient::refusal)?;
    let episode = podcasts::episode(&client, &place.episode_id)
        .await
        .map_err(|err| read_refusal(&err))?;
    if !episode.is_stored() {
        return Err(podcast_refusal(NOT_STORED));
    }
    let artwork = artwork_path(
        state,
        place.scope.profile_id,
        episode.artwork_hash.as_deref(),
    )
    .await;
    Ok((episode, artwork))
}

/// Reads the episode under the cursor again and plays it from where the server says.
/// For a step and Play from a stopped player. `Ok` without a load when a newer pick
/// overtook it during the read.
async fn play_current(app: &AppHandle, intent: LoadIntent) -> AppResult<()> {
    let state = app.state::<AppState>();
    let Some(entry) = state.podcast_playback.current() else {
        return Ok(());
    };
    let EpisodeHome::Server(place) = &entry.home else {
        return Ok(());
    };
    let (episode, artwork) = read_for_player(&state, place).await?;
    let engine = app.state::<Arc<AudioEngine>>();
    let _publish = engine.lock_publish().await;
    // A newer pick, or a session gone or moved on during the read: not this load's
    // place any more, and nothing it says should reach the OS overlay.
    let still_here = state
        .podcast_playback
        .current()
        .is_some_and(|now| now.home == entry.home);
    if engine.load_intent_superseded(intent) || !still_here {
        return Ok(());
    }
    let load = load_of(&episode, &place.scope, artwork);
    announce(app, &load);
    engine.send(AudioCmd::LoadEpisodeAndPlay {
        intent,
        episode: Box::new(load),
    })
}

/// Steps the cursor and plays what it lands on. The server keeps no played state, so
/// an episode that ended leads to the next stored one, heard or not.
pub(crate) async fn advance(app: &AppHandle, step: Step, intent: LoadIntent) -> AppResult<bool> {
    let state = app.state::<AppState>();
    let repeat = match state.require_profile_pool().await {
        Ok(pool) => crate::queue::read_repeat_mode(&pool).await,
        Err(_) => RepeatMode::Off,
    };
    let engine = app.state::<Arc<AudioEngine>>();
    {
        // A step overtaken by a newer pick moves nothing (#632). Nor does one that finds
        // another queue where it found a server's: that cursor is not its to move.
        let _publish = engine.lock_publish().await;
        if !is_server_session(&state) || !engine.claim_dispatch(intent) {
            return Ok(false);
        }
        let direction = match step {
            Step::Asked(direction) => direction,
            Step::Ended => Direction::Next,
        };
        if state.podcast_playback.step(direction, repeat).is_none() {
            let _ = engine.send(AudioCmd::Stop);
            return Ok(false);
        }
    }
    if let Err(err) = play_current(app, intent).await {
        return if give_up(app, intent).await {
            Err(err)
        } else {
            Ok(false)
        };
    }
    Ok(true)
}

/// A step whose episode could not be read stops the player and drops the session,
/// unless something newer has taken over meanwhile: then that is what the person can
/// hear, and this failure is nobody's business. `true` when it was still this step's.
async fn give_up(app: &AppHandle, intent: LoadIntent) -> bool {
    let engine = app.state::<Arc<AudioEngine>>();
    let _publish = engine.lock_publish().await;
    if engine.load_intent_superseded(intent) {
        return false;
    }
    let _ = engine.send(AudioCmd::Stop);
    app.state::<AppState>().podcast_playback.clear();
    true
}

/// The server's episode at `index` of the session, clamped as `seek_to` clamps it.
/// `None` with no session, or when that row is not a server's.
pub(crate) fn jump_target(session: &PodcastPlayback, index: usize) -> Option<ServerEpisodeRef> {
    let (entries, _) = session.snapshot()?;
    let last = entries.len().checked_sub(1)?;
    match &entries.get(index.min(last))?.home {
        EpisodeHome::Server(place) => Some((**place).clone()),
        EpisodeHome::Local { .. } => None,
    }
}

/// Moves the cursor for a jump that prepared `place`, under the publish lock.
///
/// In this order. The session is looked at first: the row was read outside the lock, and
/// another queue may have been installed since, whose cursor is not this click's to move.
/// A jump that finds that has no business claiming either, which would only overtake what
/// is on its way for that queue. Then its turn, and only then the cursor (#632).
pub(crate) fn claim_jump(
    session: &PodcastPlayback,
    index: usize,
    place: &ServerEpisodeRef,
    claim: impl FnOnce() -> bool,
) -> bool {
    if jump_target(session, index).as_ref() != Some(place) {
        return false;
    }
    claim() && session.seek_to(index).is_some()
}

/// Goes to `index` of the queue: the queue panel's click.
///
/// Reads first, since nothing has to move to know which row was clicked, so a refusal
/// changes nothing and what plays keeps playing. The claim and everything it authorizes
/// then happen under one hold of the publish lock.
pub(crate) async fn jump_to(app: &AppHandle, index: usize) -> AppResult<()> {
    let engine = app.state::<Arc<AudioEngine>>();
    let intent = engine.next_load_intent();
    let state = app.state::<AppState>();
    let Some(place) = jump_target(&state.podcast_playback, index) else {
        return Ok(());
    };
    let (episode, artwork) = read_for_player(&state, &place).await?;

    let _publish = engine.lock_publish().await;
    if !claim_jump(&state.podcast_playback, index, &place, || {
        engine.claim_dispatch(intent)
    }) {
        tracing::debug!("server episode jump overtaken; moving nothing");
        return Ok(());
    }
    let load = load_of(&episode, &place.scope, artwork);
    announce(app, &load);
    engine
        .send(AudioCmd::LoadEpisodeAndPlay {
            intent,
            episode: Box::new(load),
        })
        .inspect_err(|_| {
            let _ = engine.send(AudioCmd::Stop);
            state.podcast_playback.clear();
        })
}

/// Play from a stopped player while a server's episode is the session: from where the
/// server says it was left. `false` when that server is no longer this profile's, say
/// after signing out: the session is dropped, and the caller goes on with what Play
/// would otherwise do, rather than refusing at every press.
pub(crate) async fn resume(app: &AppHandle, intent: LoadIntent) -> AppResult<bool> {
    let state = app.state::<AppState>();
    let Some(entry) = state.podcast_playback.current() else {
        return Ok(false);
    };
    let EpisodeHome::Server(place) = &entry.home else {
        return Ok(false);
    };
    let still_bound = match state
        .require_profile_pool_for(Some(place.scope.profile_id))
        .await
    {
        Ok(pool) => {
            may_ask(&pool, crate::offline::is_offline()).await?;
            let mut conn = pool.acquire().await?;
            // The two rows and no request: a server that is merely not answering is
            // still this one, and the read below says so.
            place.scope.is_current(&mut conn).await?
        }
        Err(_) => false,
    };
    if !still_bound {
        state.podcast_playback.clear();
        return Ok(false);
    }
    let engine = app.state::<Arc<AudioEngine>>();
    {
        let _publish = engine.lock_publish().await;
        if !engine.claim_dispatch(intent) {
            return Ok(true);
        }
    }
    play_current(app, intent).await?;
    Ok(true)
}

/// What a position comes to on the server: a bookmark, or none once it counts as
/// heard. Pure, so the rule is tested without one.
pub(crate) fn position_write(position_ms: u64, duration_ms: u64, finished: bool) -> Option<u64> {
    if finished || super::playback::counts_as_played(position_ms, duration_ms) {
        None
    } else {
        Some(position_ms)
    }
}

/// Writes where the episode stands to the account that was listening, or to nobody.
/// With the switch off or offline mode on nothing is sent, so a position not yet
/// written is lost: there is no local row to hold it for later, and it is never kept
/// as one. Failures are the same, and are only logged by their status.
pub(crate) async fn record_position(
    app: &AppHandle,
    place: &ServerEpisodeRef,
    position_ms: u64,
    duration_ms: u64,
    finished: bool,
    announce_it: bool,
) {
    let state = app.state::<AppState>();
    {
        let Ok(pool) = state
            .require_profile_pool_for(Some(place.scope.profile_id))
            .await
        else {
            tracing::debug!("podcast: server position dropped, its profile is gone");
            return;
        };
        if may_ask(&pool, crate::offline::is_offline()).await.is_err() {
            return;
        }
    }
    let write = {
        let (app, place) = (app.clone(), place.clone());
        async move {
            let state = app.state::<AppState>();
            let _order = POSITION_WRITES.lock().await;
            let client = match client_in_scope(&state, &place.scope).await {
                Ok(client) => client,
                Err(why) => {
                    tracing::debug!(?why, "podcast: server position dropped");
                    return;
                }
            };
            let written = match position_write(position_ms, duration_ms, finished) {
                Some(position) => {
                    podcasts::save_position(&client, &place.episode_id, position).await
                }
                None => podcasts::clear_position(&client, &place.episode_id).await,
            };
            if let Err(err) = written {
                tracing::debug!(status = ?err.status, "podcast: server position not written");
            }
        }
    };
    // This runs on the task that also steps to the next episode, and a token refresh
    // on a server that hangs would otherwise take the whole of its own deadline. Only
    // the wait gives up: see [`detached`].
    if tokio::time::timeout(POSITION_BUDGET, detached(write))
        .await
        .is_err()
    {
        tracing::debug!("podcast: server position not written in time");
    }
    if announce_it {
        let _ = app.emit(
            PODCAST_SERVER_UPDATED,
            serde_json::json!({ "channel_id": place.channel_id }),
        );
    }
}

/// A speed moved during a server's episode: heard at once, and kept for the channel
/// while the app runs.
pub(crate) fn set_channel_speed(
    app: &AppHandle,
    place: &ServerEpisodeRef,
    speed: f32,
) -> AppResult<()> {
    if let Ok(mut speeds) = SPEEDS.lock() {
        speeds.insert((place.scope.profile_id, place.channel_id.clone()), speed);
    }
    app.state::<Arc<AudioEngine>>()
        .send(AudioCmd::SetEpisodeSpeed(speed))
}

/// Asked before every request of a stream: the switch, offline mode, and whether this
/// is still the server the profile is bound to.
async fn may_stream(app: &AppHandle, scope: &ServerScope) -> Result<(), StreamRefusal> {
    if crate::offline::is_offline() {
        return Err(StreamRefusal::Offline);
    }
    let state = app.state::<AppState>();
    let pool = state
        .require_profile_pool_for(Some(scope.profile_id))
        .await
        .map_err(|_| StreamRefusal::Released)?;
    if !store::is_enabled(&pool).await.unwrap_or(false) {
        return Err(StreamRefusal::Disabled);
    }
    let mut conn = pool.acquire().await.map_err(|_| StreamRefusal::Released)?;
    match scope.is_current(&mut conn).await {
        Ok(true) => Ok(()),
        // Nothing here is for whoever is bound now, so nothing to say to them.
        Ok(false) | Err(_) => Err(StreamRefusal::Released),
    }
}

/// A ticket for the episode, as an address.
async fn mint(
    app: &AppHandle,
    scope: &ServerScope,
    episode_id: &str,
) -> Result<Url, StreamRefusal> {
    let state = app.state::<AppState>();
    let client = client_in_scope(&state, scope)
        .await
        .map_err(NoClient::stream_refusal)?;
    let address = mint_ticket(&client, episode_id)
        .await
        .map_err(|err| match err.status {
            // Stored a moment ago and no longer, or the server's own ticket rules.
            Some(404) => StreamRefusal::NotStored,
            _ => StreamRefusal::Failed(FetchFailure::Unreached),
        })?;
    parse_address(&address).ok_or(StreamRefusal::Failed(FetchFailure::AddressRefused))
}

/// Opens a server's episode for the decoder. Blocking: for its thread alone.
pub(crate) fn open_stream(
    app: &AppHandle,
    place: &ServerEpisodeRef,
) -> Result<EnclosureSource, StreamRefusal> {
    let fetcher = Fetcher::new().map_err(StreamRefusal::from)?;
    let gate: Gate = {
        let (app, scope) = (app.clone(), place.scope.clone());
        Arc::new(move || {
            let (app, scope) = (app.clone(), scope.clone());
            Box::pin(async move { may_stream(&app, &scope).await })
        })
    };
    let renew: Renew = {
        let (app, scope, episode_id) = (app.clone(), place.scope.clone(), place.episode_id.clone());
        Arc::new(move || {
            let (app, scope, episode_id) = (app.clone(), scope.clone(), episode_id.clone());
            // Under the stream's deadline and a Stop's letting go: see [`detached`].
            Box::pin(async move {
                detached(async move { mint(&app, &scope, &episode_id).await })
                    .await
                    .unwrap_or(Err(StreamRefusal::Released))
            })
        })
    };
    EnclosureSource::open_renewing(
        tauri::async_runtime::handle().inner().clone(),
        fetcher,
        gate,
        renew,
        place.scope.origin(),
    )
}

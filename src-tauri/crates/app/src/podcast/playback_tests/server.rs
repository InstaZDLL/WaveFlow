//! A bound server's episode: its queue, its position, a ticket that runs out.

use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::runtime::Handle;
use waveflow_core::podcast::FetchFailure;

use super::super::playback::{
    EpisodeEntry, EpisodeHome, EpisodeLoad, EpisodeSource, PodcastPlayback,
};
use super::super::refresh::PodcastError;
use super::super::server::{
    claim_jump, detached, jump_target, may_ask, position_write, queue_of, NoClient,
    ServerEpisodeRef,
};
use super::super::store;
use super::super::stream::{EnclosureSource, Gate, Renew, StreamRefusal};
use super::super::tests::{enabled_pool, fetcher, serve, url, with_status, Reply};
use super::open_gate;
use crate::audio::analytics::AnalyticsMsg;
use crate::audio::engine::{AudioCmd, LastLoad, LoadIntent};
use crate::audio::state::SharedPlayback;
use crate::remote::podcasts::{ServerEpisode, ServerScope};
use crate::remote::test_support::scope_on;
use crate::remote_playback::DetachedQueue;

const TICKET: &str = "sealed-ticket-9c2e";

fn episode(id: &str, status: &str) -> ServerEpisode {
    ServerEpisode {
        id: id.to_owned(),
        channel_id: "c1".to_owned(),
        channel_title: "Show".to_owned(),
        artwork_hash: None,
        title: format!("Episode {id}"),
        description: None,
        published_at: None,
        duration_ms: Some(600_000),
        status: status.to_owned(),
        error: None,
        size: None,
        content_type: None,
        position_ms: None,
    }
}

fn scope() -> ServerScope {
    scope_on("https://music.example", "acc-1")
}

fn place(episode_id: &str) -> ServerEpisodeRef {
    ServerEpisodeRef {
        scope: scope(),
        episode_id: episode_id.to_owned(),
        channel_id: "c1".to_owned(),
    }
}

fn ids(entries: &[super::super::playback::EpisodeEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| match &entry.home {
            EpisodeHome::Server(place) => place.episode_id.clone(),
            EpisodeHome::Local { .. } => panic!("a local entry in a server's queue"),
        })
        .collect()
}

// ── the queue ───────────────────────────────────────────────────────

#[test]
fn the_queue_holds_what_the_server_stored_and_in_its_order() {
    let episodes = [
        episode("e1", "completed"),
        episode("e2", "new"),
        episode("e3", "completed"),
        episode("e4", "downloading"),
        episode("e5", "error"),
        episode("e6", "completed"),
    ];
    let (entries, index) = queue_of(&episodes, "e3", &scope()).unwrap();
    assert_eq!(ids(&entries), ["e1", "e3", "e6"]);
    assert_eq!(index, 1);
}

#[test]
fn an_episode_with_no_file_on_the_server_cannot_start() {
    let episodes = [episode("e1", "completed"), episode("e2", "new")];
    for status in ["new", "downloading", "error", "deleted"] {
        let episodes = [episodes[0].clone(), episode("e2", status)];
        assert_eq!(
            queue_of(&episodes, "e2", &scope()).unwrap_err(),
            "not_stored",
            "{status}"
        );
    }
    assert_eq!(
        queue_of(&episodes, "gone", &scope()).unwrap_err(),
        "episode_gone"
    );
}

// ── a jump in the queue ─────────────────────────────────────────────

fn server_entry(id: &str) -> EpisodeEntry {
    EpisodeEntry {
        home: EpisodeHome::Server(Box::new(place(id))),
        profile_id: 1,
        title: id.to_owned(),
        channel: "Show".to_owned(),
        duration_ms: None,
    }
}

fn local_entry(id: i64) -> EpisodeEntry {
    EpisodeEntry {
        home: EpisodeHome::Local {
            episode_id: id,
            channel_id: 1,
        },
        profile_id: 1,
        title: id.to_string(),
        channel: "Show".to_owned(),
        duration_ms: None,
    }
}

fn session_of(entries: Vec<EpisodeEntry>) -> PodcastPlayback {
    let session = PodcastPlayback::default();
    session.set(DetachedQueue { entries, index: 0 });
    session
}

fn cursor(session: &PodcastPlayback) -> usize {
    session.snapshot().expect("a session").1
}

#[test]
fn a_jump_names_the_servers_row_it_was_asked_for_and_nothing_else() {
    let servers = session_of(vec![server_entry("e1"), server_entry("e2")]);
    assert_eq!(jump_target(&servers, 1), Some(place("e2")));
    // Past the end lands on the last row, as moving the cursor does.
    assert_eq!(jump_target(&servers, 9), Some(place("e2")));
    assert_eq!(jump_target(&PodcastPlayback::default(), 0), None);
    assert_eq!(jump_target(&session_of(Vec::new()), 0), None);
    assert_eq!(jump_target(&session_of(vec![local_entry(1)]), 0), None);
}

#[test]
fn a_jump_with_its_turn_moves_the_cursor() {
    let session = session_of(vec![server_entry("e1"), server_entry("e2")]);
    let mut asked = 0;
    let moved = claim_jump(&session, 1, &place("e2"), || {
        asked += 1;
        true
    });
    assert!(moved);
    assert_eq!(asked, 1);
    assert_eq!(cursor(&session), 1);
}

/// The row was read outside the lock. Whatever is installed since is another queue, whose
/// cursor is not this click's, and whose load must not be overtaken by a claim that
/// sends nothing.
#[test]
fn a_jump_that_finds_another_queue_neither_claims_nor_moves_it() {
    let prepared = place("e2");
    let never = || -> bool { panic!("a jump for another queue asked for its turn") };

    let local = session_of(vec![local_entry(1), local_entry(2)]);
    assert!(!claim_jump(&local, 1, &prepared, never));
    assert_eq!(cursor(&local), 0);

    let other_channel = session_of(vec![server_entry("e8"), server_entry("e9")]);
    assert!(!claim_jump(&other_channel, 1, &prepared, never));
    assert_eq!(cursor(&other_channel), 0);

    // The same ids on another server, or for another account, are other episodes.
    let elsewhere = session_of(vec![
        server_entry("e1"),
        EpisodeEntry {
            home: EpisodeHome::Server(Box::new(ServerEpisodeRef {
                scope: scope_on("https://music.example", "acc-2"),
                episode_id: "e2".to_owned(),
                channel_id: "c1".to_owned(),
            })),
            ..server_entry("e2")
        },
    ]);
    assert!(!claim_jump(&elsewhere, 1, &prepared, never));
    assert_eq!(cursor(&elsewhere), 0);

    let gone = PodcastPlayback::default();
    assert!(!claim_jump(&gone, 1, &prepared, never));
}

#[test]
fn a_jump_overtaken_during_its_read_moves_nothing() {
    let shared = SharedPlayback::new();
    let jump = LoadIntent::claim(&shared);
    let newer = LoadIntent::claim(&shared);
    assert!(shared.try_claim_load(newer.get()));

    let session = session_of(vec![server_entry("e1"), server_entry("e2")]);
    let moved = claim_jump(&session, 1, &place("e2"), || {
        shared.try_claim_load(jump.get())
    });
    assert!(!moved);
    assert_eq!(cursor(&session), 0);
}

// ── no client ───────────────────────────────────────────────────────

/// A refresh that got no answer is a server not answering. Taken for a change of profile,
/// it read as a wrong sentence, and ended a stream without a word.
#[test]
fn a_network_gone_during_a_refresh_is_not_a_change_of_profile() {
    assert_eq!(
        NoClient::Unreachable.refusal().to_string(),
        "podcast: server_unreachable"
    );
    assert_eq!(
        NoClient::ScopeGone.refusal().to_string(),
        "podcast: profile_changed"
    );
    assert_eq!(
        NoClient::Unreachable.stream_refusal(),
        StreamRefusal::Failed(FetchFailure::Unreached)
    );
    // Quiet: the decoder knows this one, and it is nobody's fault here.
    assert_eq!(
        NoClient::ScopeGone.stream_refusal(),
        StreamRefusal::Released
    );
}

// ── a wait given up on ──────────────────────────────────────────────

/// A token refresh may be inside: the old token is spent the moment the server is
/// asked, so what comes back has to reach the database whoever stopped listening.
#[tokio::test]
async fn giving_up_on_a_wait_does_not_stop_the_work_behind_it() {
    let finished = Arc::new(AtomicUsize::new(0));
    let flag = Arc::clone(&finished);
    let waited = tokio::time::timeout(
        std::time::Duration::from_millis(20),
        detached(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            flag.store(1, Ordering::SeqCst);
        }),
    )
    .await;
    assert!(waited.is_err(), "the wait gave up");
    assert_eq!(finished.load(Ordering::SeqCst), 0);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert_eq!(finished.load(Ordering::SeqCst), 1, "the work went on");
}

// ── the position ────────────────────────────────────────────────────

#[test]
fn a_position_past_the_point_of_having_heard_it_clears_the_bookmark() {
    let ten_minutes = 600_000;
    assert_eq!(position_write(90_000, ten_minutes, false), Some(90_000));
    // 95 %, and the last 30 seconds: the rule local episodes go by.
    assert_eq!(position_write(570_000, ten_minutes, false), None);
    assert_eq!(position_write(569_999, ten_minutes, false), Some(569_999));
    // An end is an end, whatever the server said the length was.
    assert_eq!(position_write(90_000, ten_minutes, true), None);
    // A length nobody knows says nothing.
    assert_eq!(position_write(90_000, 0, false), Some(90_000));
    assert_eq!(position_write(90_000, 0, true), None);
}

// ── the switch, on the migrated schema ──────────────────────────────

#[tokio::test]
async fn no_request_leaves_with_the_switch_off_or_offline() {
    let pool = enabled_pool().await;
    assert!(may_ask(&pool, false).await.is_ok());
    assert!(matches!(
        may_ask(&pool, true).await,
        Err(PodcastError::Offline)
    ));
    sqlx::query("UPDATE profile_setting SET value = 'false' WHERE key = ?")
        .bind(store::ENABLED_KEY)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        may_ask(&pool, false).await,
        Err(PodcastError::Disabled)
    ));
}

#[tokio::test]
async fn a_new_profile_has_the_switch_off_for_a_server_too() {
    let pool = enabled_pool().await;
    sqlx::query("DELETE FROM profile_setting WHERE key = ?")
        .bind(store::ENABLED_KEY)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        may_ask(&pool, false).await,
        Err(PodcastError::Disabled)
    ));
}

// ── a ticket that runs out ──────────────────────────────────────────

/// Far enough that the seek is a request and not a read through.
const FAR: u64 = 1_000_000;
const TOTAL: u64 = 10_000_000;

fn first_bytes() -> Reply {
    with_status(
        "206 Partial Content",
        &format!("Content-Type: audio/mpeg\r\nContent-Range: bytes 0-9/{TOTAL}\r\n"),
        b"0123456789",
    )
}

fn far_bytes() -> Reply {
    with_status(
        "206 Partial Content",
        &format!(
            "Content-Type: audio/mpeg\r\nContent-Range: bytes {FAR}-{}/{TOTAL}\r\n",
            FAR + 9
        ),
        b"abcdefghij",
    )
}

fn missing() -> Reply {
    with_status("404 Not Found", "", b"{}")
}

fn renew_with(addresses: Vec<Result<String, StreamRefusal>>) -> (Renew, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&asked);
    let renew: Renew = Arc::new(move || {
        let turn = counter.fetch_add(1, Ordering::SeqCst);
        let answer = addresses[turn.min(addresses.len() - 1)].clone();
        Box::pin(async move { answer.map(|raw| url(&raw)) })
    });
    (renew, asked)
}

/// Opens on a blocking thread, as the decoder's is.
async fn open_renewing(
    renew: Renew,
    gate: Gate,
    origin_of: &str,
) -> Result<EnclosureSource, StreamRefusal> {
    let runtime = Handle::current();
    let origin = super::super::fetch::Origin::of(&url(origin_of));
    tokio::task::spawn_blocking(move || {
        EnclosureSource::open_renewing(runtime, fetcher(), gate, renew, origin)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_ticket_that_still_answers_is_not_replaced() {
    let server = serve(|_: &str| first_bytes()).await;
    let first = server.local("/api/v2/stream/first");
    let (renew, asked) = renew_with(vec![Ok(first.clone())]);
    let source = open_renewing(renew, open_gate(), &first).await.unwrap();
    assert_eq!(asked.load(Ordering::SeqCst), 1, "minted when it opened");

    tokio::task::spawn_blocking(move || {
        let mut source = source;
        let mut sink = [0u8; 4];
        source.read_exact(&mut sink).unwrap();
        assert_eq!(&sink, b"0123");
        // Answered with the wrong bytes rather than a 404: not a ticket running out.
        source.seek(SeekFrom::Start(FAR)).unwrap_err();
    })
    .await
    .unwrap();
    assert_eq!(asked.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_expired_ticket_is_replaced_once_and_the_episode_goes_on() {
    let turns = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&turns);
    let server = serve(move |path: &str| {
        if path.starts_with("/api/v2/stream/first") {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                first_bytes()
            } else {
                // What a ticket past its hour answers: it is told from an unknown
                // track by nothing.
                missing()
            }
        } else {
            far_bytes()
        }
    })
    .await;
    let first = server.local("/api/v2/stream/first");
    let second = server.local("/api/v2/stream/second");
    let (renew, asked) = renew_with(vec![Ok(first.clone()), Ok(second)]);
    let source = open_renewing(renew, open_gate(), &first).await.unwrap();

    tokio::task::spawn_blocking(move || {
        let mut source = source;
        assert_eq!(source.seek(SeekFrom::Start(FAR)).unwrap(), FAR);
        let mut sink = [0u8; 4];
        source.read_exact(&mut sink).unwrap();
        assert_eq!(&sink, b"abcd");
    })
    .await
    .unwrap();
    assert_eq!(asked.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_file_gone_from_the_server_says_not_stored_at_the_seek() {
    let turns = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&turns);
    let server = serve(move |_path: &str| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            first_bytes()
        } else {
            missing()
        }
    })
    .await;
    let first = server.local("/api/v2/stream/first");
    let (renew, _) = renew_with(vec![Ok(first.clone()), Err(StreamRefusal::NotStored)]);
    let source = open_renewing(renew, open_gate(), &first).await.unwrap();
    let error = tokio::task::spawn_blocking(move || {
        let mut source = source;
        source.seek(SeekFrom::Start(FAR)).unwrap_err()
    })
    .await
    .unwrap();
    assert_eq!(error.to_string(), "podcast: not_stored");
}

#[tokio::test]
async fn an_episode_not_stored_opens_to_that_word_and_contacts_no_stream() {
    let server = serve(|_: &str| first_bytes()).await;
    let origin = server.local("/");
    let (renew, _) = renew_with(vec![Err(StreamRefusal::NotStored)]);
    let refusal = open_renewing(renew, open_gate(), &origin)
        .await
        .err()
        .expect("nothing to open");
    assert_eq!(refusal.message(), "podcast: not_stored");
    assert_eq!(server.hits(), 0);
}

#[tokio::test]
async fn the_switch_holds_ahead_of_the_first_ticket() {
    let server = serve(|_: &str| first_bytes()).await;
    let origin = server.local("/");
    let (renew, asked) = renew_with(vec![Ok(origin.clone())]);
    let off: Gate = Arc::new(|| Box::pin(async { Err(StreamRefusal::Disabled) }));
    let refusal = open_renewing(renew, off, &origin)
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal, StreamRefusal::Disabled);
    assert_eq!(asked.load(Ordering::SeqCst), 0, "no ticket was minted");
    assert_eq!(server.hits(), 0);
}

// ── nothing that is printed holds an address ────────────────────────

#[test]
fn a_load_and_what_it_leaves_behind_print_no_ticket_and_no_server() {
    let load = EpisodeLoad {
        home: EpisodeHome::Server(Box::new(place("e1"))),
        profile_id: 1,
        track_id: -1,
        start_ms: 0,
        duration_ms: 0,
        title: "One".to_owned(),
        channel: "Show".to_owned(),
        artwork_path: None,
        speed: 1.0,
        source: EpisodeSource::Server {
            declared_type: None,
        },
    };
    let command = AudioCmd::LoadEpisodeAndPlay {
        intent: LoadIntent::from_raw(1),
        episode: Box::new(load.clone()),
    };
    let captured = LastLoad::capture(&command).expect("a load");
    let left = AnalyticsMsg::EpisodeLeft {
        profile_id: 1,
        home: load.home.clone(),
        position_ms: 1,
        duration_ms: 1,
        finished: false,
    };
    for printed in [
        format!("{command:?}"),
        format!("{captured:?}"),
        format!("{left:?}"),
    ] {
        assert!(!printed.contains("music.example"), "{printed}");
        assert!(!printed.contains(TICKET), "{printed}");
        assert!(!printed.contains("acc-1"), "{printed}");
    }
}

/// What a stream's failures say to a log is the same fixed words whatever the ticket.
#[tokio::test]
async fn no_refusal_of_a_stream_quotes_its_ticket() {
    let server = serve(|_: &str| missing()).await;
    let ticketed = server.local(&format!("/api/v2/stream/{TICKET}"));
    let (renew, _) = renew_with(vec![Ok(ticketed.clone())]);
    let refusal = open_renewing(renew, open_gate(), &ticketed)
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.message(), "podcast: http_404");
    assert!(!format!("{refusal:?}").contains(TICKET));
}

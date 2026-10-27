use super::*;
use tokio_util::sync::CancellationToken;

#[test]
fn parses_known_subsystems_and_rejects_the_rest() {
    assert_eq!(Subsystem::parse("player"), Some(Subsystem::Player));
    assert_eq!(Subsystem::parse("mixer"), Some(Subsystem::Mixer));
    // We never fire `database`, so claiming to support it would
    // leave a client idling on it forever.
    assert_eq!(Subsystem::parse("database"), None);
    assert_eq!(Subsystem::parse("nonsense"), None);
}

#[tokio::test]
async fn wakes_on_a_wanted_subsystem() {
    let bus = IdleBus::new();
    let mut rx = bus.subscribe();
    bus.notify(Subsystem::Player);
    let hits = wait(&mut rx, &[Subsystem::Player], &CancellationToken::new()).await;
    assert_eq!(hits, vec![Subsystem::Player]);
}

#[tokio::test]
async fn ignores_subsystems_the_client_did_not_ask_for() {
    let bus = IdleBus::new();
    let mut rx = bus.subscribe();
    bus.notify(Subsystem::Mixer);
    bus.notify(Subsystem::Player);
    // Only `player` was requested, so the mixer event must not wake
    // this client.
    let hits = wait(&mut rx, &[Subsystem::Player], &CancellationToken::new()).await;
    assert_eq!(hits, vec![Subsystem::Player]);
}

#[tokio::test]
async fn coalesces_a_burst_into_one_answer() {
    let bus = IdleBus::new();
    let mut rx = bus.subscribe();
    // A track change moves both at once; the client should be woken
    // once carrying both, not twice.
    bus.notify(Subsystem::Player);
    bus.notify(Subsystem::Playlist);
    let hits = wait(
        &mut rx,
        &[Subsystem::Player, Subsystem::Playlist],
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(hits.len(), 2);
    assert!(hits.contains(&Subsystem::Player));
    assert!(hits.contains(&Subsystem::Playlist));
}

#[tokio::test]
async fn cancellation_returns_empty() {
    let bus = IdleBus::new();
    let mut rx = bus.subscribe();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let hits = wait(&mut rx, &Subsystem::ALL, &cancel).await;
    assert!(hits.is_empty());
}

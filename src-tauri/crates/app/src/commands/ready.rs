//! The splash → main handoff rendezvous (#626).
//!
//! The main window is created with `visible: false` and revealed from
//! native code once the frontend says it has rendered. A 15 s safety net
//! reveals it anyway if that signal never arrives — and that net fires
//! regularly on Windows, leaving the user on the splash for a full fifteen
//! seconds.
//!
//! ## Why the signal lives here rather than in `setup`
//!
//! It used to be an event: the frontend `emit`s `app://ready` and `setup`
//! registers an `app.listen` for it. Both halves of that depend on `setup`
//! having got far enough. The `main` window is declared in
//! `tauri.conf.json`, so its webview is created by `Builder::build` and is
//! already loading — and already able to call into the IPC — while `setup`
//! is still doing three blocking database reads and opening the audio
//! device. Events are not replayed, so a signal that is handled before the
//! listener exists is gone for good and the net is all that is left.
//!
//! A **command** cannot be missed that way: `generate_handler!` registers
//! it at build time, before any window exists. And
//! [`tokio::sync::Notify::notify_one`] keeps a permit when it fires before
//! the reveal task awaits, so arriving early is safe rather than fatal.
//!
//! The event listener stays as a second transport. This is a one-shot
//! rendezvous that ignores duplicates, so two independent ways to reach it
//! cost nothing and the splash is not worth a single point of failure.
//!
//! ## What is still not known
//!
//! Nobody has measured which half of the handoff is slow, so both sides now
//! report their own timing: the frontend sends how long it took to render
//! since navigation, and this module logs that next to the elapsed time
//! since launch. A signal that arrives at 14 s with
//! `since_navigation_ms` close to it means the frontend was genuinely late
//! — `main.tsx` gates its first render on i18next, whose locale chunk is
//! fetched through the asset protocol that `setup` is blocking — while one
//! that never arrives at all points back at the transport.

use std::sync::OnceLock;
use std::time::Instant;

use tokio::sync::Notify;

/// The rendezvous itself: the signal, and when the process started.
struct ReadyGate {
    notify: Notify,
    launched_at: Instant,
}

static GATE: OnceLock<ReadyGate> = OnceLock::new();

fn gate() -> &'static ReadyGate {
    GATE.get_or_init(|| ReadyGate {
        notify: Notify::new(),
        launched_at: Instant::now(),
    })
}

/// Start the launch clock. Called first thing in `run`, so the elapsed
/// times logged below are measured from the process start rather than from
/// whichever half of the handoff happened to touch the gate first.
pub fn mark_launch() {
    let _ = gate();
}

/// Milliseconds since [`mark_launch`].
pub fn since_launch_ms() -> u128 {
    gate().launched_at.elapsed().as_millis()
}

/// Wait for the frontend's signal. Resolves immediately if it already
/// arrived — the permit is kept.
pub async fn wait() {
    gate().notify.notified().await;
}

/// Record the signal, whichever transport carried it.
///
/// `source` names that transport so a log tells the two apart, and
/// `since_navigation_ms` is the frontend's own measurement when it has one
/// (the event path carries no payload).
pub fn signal(source: &'static str, since_navigation_ms: Option<u64>) {
    tracing::info!(
        source,
        since_launch_ms = since_launch_ms(),
        since_navigation_ms,
        "splash handoff: frontend reported ready"
    );
    gate().notify.notify_one();
}

/// Called by the frontend once React has committed its first render.
///
/// `since_navigation_ms` is `performance.now()` at that moment: the time
/// the webview spent from navigation to first commit, which is the half
/// this side cannot see.
#[tauri::command]
pub fn app_ready(since_navigation_ms: Option<u64>) {
    signal("command", since_navigation_ms);
}

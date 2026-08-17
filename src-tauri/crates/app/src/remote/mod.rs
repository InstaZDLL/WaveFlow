//! Remote music source + user-data sync v2. Desktop side of
//! [RFC-005](https://github.com/InstaZDLL/WaveFlow/blob/main/docs/rfcs/RFC-005-remote-source-and-sync-v2.md).
//!
//! ## Not to be confused with [`crate::sync`]
//!
//! `crate::sync` is now a permanent no-op stub. The v1 protocol it once
//! held (peer-to-peer ops, hybrid logical clocks, digest reconciliation)
//! spoke to a server generation that no longer exists and was removed in
//! the RFC-005 cutover, once this module's snapshot bootstrap was proven.
//! This module is its replacement, not its evolution.
//!
//! Neither module's docs should be read as describing the other, and
//! the two RFCs numbered 003 (one per repository) are likewise
//! unrelated — see RFC-005 §"The RFC-003 naming trap".
//!
//! ## Module map
//!
//! - [`auth`] — Authorization Code + PKCE in the system browser, and
//!   the two ways out: sign out (credentials only) versus forget the
//!   server (credentials, binding, projection and pending writes).
//!
//! - [`binding`] — which server this profile talks to and where it is
//!   up to. Holds [`binding::RemoteIdentity`], the polymorphic identity
//!   that keeps a third-party server from having to pretend it has an
//!   account UUID, a device and a journal cursor.
//!
//! - [`tokens`] — access / refresh token persistence. Separate from the
//!   binding because tokens rotate and the binding does not.
//!
//! - [`client`] — the HTTP surface. Attaches the Bearer header, stamps
//!   mutations with their operation and device identifiers, refreshes
//!   once on a 401, and classifies every failure as permanent or
//!   transient so the outbound queue knows whether retrying can ever
//!   help.
//!
//! - [`dto`] — the wire shapes, checked against the server's live
//!   responses rather than against prose.
//!
//! - [`mutation`] — the outbound queue. Replayable business calls, each
//!   pinned to an operation identifier that must never be reused for a
//!   different intent.
//!
//! - [`write`] — local gestures on remote data. Applies the change to
//!   the projection and queues the mutation in **one** transaction;
//!   splitting them either loses the change or makes the interface lie.
//!
//! - [`drain`] — pushes that queue, in order, stopping on anything that
//!   might still succeed and marking anything that never will.
//!
//! - [`projection`] — writing the server's user data into the local
//!   `remote_*` tables. Pure database work, testable without a network.
//!
//! - [`read`] — reading it back out for the UI. Also pure: what the
//!   projection holds is already the answer, so nothing here needs the
//!   server to be reachable.
//!
//! - [`sync`] — the orchestrator: bootstrap from a snapshot, walk the
//!   journal, acknowledge, and recover by re-snapshotting when a known
//!   event cannot be applied.
//!
//! - [`socket`] — the wake-up channel. Pure latency: it carries notice
//!   that a newer cursor may exist, never state, so removing it would
//!   leave the projection correct and merely slower to notice another
//!   device's edits.
//!
//! - [`probe`] — server flavour detection. Decides whether the optional
//!   [`SyncProvider`](#capabilities) surface exists at all.
//!
//! ## Capabilities
//!
//! The desktop must talk to any server speaking the Subsonic protocol,
//! so the mandatory surface is what every server does — catalogue,
//! search, playback — and the journal is an optional capability layered
//! on top. Only WaveFlow offers it, which is why nothing in this module
//! may assume it is present without having observed it.

pub mod auth;
pub mod binding;
pub mod catalogue;
pub mod client;
pub mod drain;
pub mod dto;
pub mod lyrics;
pub mod mutation;
pub mod playback;
pub mod probe;
pub mod projection;
pub mod read;
pub mod reconciliation;
pub mod socket;
pub mod stream;
pub mod sync;
pub mod tokens;
pub mod write;

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
//! - [`mirror`] — the other half of that: walking the server's whole
//!   **catalogue** into the same tables, so both sources can be browsed
//!   from one list. The projection only ever sees the tracks the account
//!   touched; everything else exists solely on the server until this
//!   walks it in.
//!
//! - [`stream`] — minting a playback ticket, and the transcode
//!   preference that decides what the URL asks the server for.
//!
//! - [`download`] — keeping a track's original bytes in a managed folder
//!   the scanner never sees. Still a *remote* track: no `track` row.
//!
//! - [`import`] — the opposite gesture: the same bytes copied into a
//!   folder the user already scans, where they become a local track
//!   with the server's one linked to it.
//!
//! - [`upload`] — the fourth direction: offering the server what it
//!   does not have, over its RFC-008 routes. Most of that work never
//!   reaches it, since the mirror already knows which digests it holds.
//!
//! - [`hashing`] — the whole-file digest of a local track, computed
//!   once and shared by every path that needs one, because deciding
//!   what a server is missing means reading the library. Keeps two
//!   kinds of caller apart: *discovery* asks what a file hashes to and
//!   may be answered from the cache; *verification* asks whether it
//!   still hashes to that, immediately before an irreversible write,
//!   and always reads the file.
//!
//! - [`reconciliation`] — pairing local files with server tracks after
//!   the fact, which costs a full re-read; [`import`] is the one path
//!   that gets the same proof for nothing.
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

pub mod artwork;
pub mod auth;
pub mod binding;
pub mod catalogue;
pub mod client;
pub mod download;
pub mod drain;
pub mod dto;
pub mod hashing;
pub mod import;
pub mod lyrics;
pub mod mirror;
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
pub mod upload;
pub mod write;

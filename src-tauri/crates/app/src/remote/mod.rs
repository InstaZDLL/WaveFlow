//! Remote music source + user-data sync v2. Desktop side of
//! [RFC-005](https://github.com/InstaZDLL/WaveFlow/blob/main/docs/rfcs/RFC-005-remote-source-and-sync-v2.md).
//!
//! ## Not to be confused with [`crate::sync`]
//!
//! `crate::sync` implements the retired v1 protocol (peer-to-peer ops,
//! hybrid logical clocks, digest reconciliation) against a server that
//! no longer exists. It stays in the tree, behind the `sync_v1`
//! feature, for exactly as long as it takes to prove the snapshot
//! bootstrap here — until then it is the only path back from a
//! divergence. This module is its replacement, not its evolution.
//!
//! Neither module's docs should be read as describing the other, and
//! the two RFCs numbered 003 (one per repository) are likewise
//! unrelated — see RFC-005 §"The RFC-003 naming trap".
//!
//! ## Module map
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

// This slice lands the foundation; its consumers — the PKCE handshake,
// the snapshot bootstrap, the outbound drain — land in the ones that
// follow. Until then most of the surface below has no caller and
// `dead_code` would flag nearly all of it, drowning the warnings that
// mean something. Same treatment, and the same reason, as
// `sync/mode.rs`. Drop this once the drain consumes the client.
#![allow(dead_code)]

pub mod binding;
pub mod client;
pub mod probe;
pub mod tokens;

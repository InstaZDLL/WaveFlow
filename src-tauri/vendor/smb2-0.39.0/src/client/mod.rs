//! High-level SMB2 client API.
//!
//! Provides [`SmbClient`] for easy connect-and-use access, plus lower-level
//! types: [`Connection`] for message exchange, [`Session`] for authenticated
//! sessions, [`Tree`] for share access with file operations, and [`Pipeline`]
//! for batched concurrent operations.

pub mod batch;
pub mod connection;
mod connections;
pub mod copy;
pub(crate) mod credits;
pub(crate) mod dfs;
mod dfs_policy;
pub mod diagnostics;
pub mod directory;
#[cfg(test)]
mod download_tests;
pub mod durable;
#[cfg(test)]
mod fault_injection_tests;
pub mod pipeline;
pub mod read_ahead;
pub mod resolve;
#[cfg(test)]
mod resume_tests;
pub mod session;
pub mod shares;
#[cfg(all(test, feature = "smol"))]
mod smol_runtime_tests;
#[cfg(test)]
mod socket_lifecycle_tests;
#[cfg(test)]
mod stale_handle_tests;
pub mod stream;
pub mod streams;
#[cfg(test)]
pub(crate) mod test_helpers;
pub mod times;
pub mod tree;
pub(crate) mod tuning;
#[cfg(test)]
mod upload_tests;
pub mod watcher;
pub mod write_behind;
pub(crate) mod write_pipe;
pub mod xattr;

pub use crate::crypto::encryption::Cipher;
pub use batch::{BatchOptions, BatchResults};
pub use connection::{
    CompoundOp, Connection, Frame, NegotiatedParams, ReconnectEvent, ReconnectObserver,
    ReconnectPolicy, SessionReviver,
};
pub use dfs_policy::{DfsSignIn, DfsTargetPolicy};
pub use diagnostics::{
    ClientInfo, ClientMetricsSnapshot, CompressionInfo, ConnectionDiagnostics, CreditInfo,
    DfsCacheEntry, Diagnostics, EncryptionInfo, InboundProgress, Liveness, MetricsSnapshot,
    NegotiatedSummary, SessionDiagnostics, SigningInfo,
};
pub use directory::{DirectoryReader, ListOptions};
pub use durable::{DurableHandle, DurableOpen, FileIdentity};
pub use pipeline::{Op, OpResult, Pipeline};
pub use resolve::Resolved;
pub use session::Session;
pub use shares::list_shares;
pub use stream::{FileDownload, FileUpload, FileWriter, Progress, WriteCheckpoint};
pub use streams::{FsAttributes, StreamInfo};
pub use times::FileTimes;
pub use tree::{
    DfsLink, DfsOrigin, DirectoryEntry, FileInfo, FsInfo, ListingTrace, QueryStep, RenameOptions,
    Tree,
};
pub use watcher::{FileNotifyAction, FileNotifyEvent, WatchOptions, Watcher};

// Re-export high-level client types.
// (SmbClient, ClientConfig, and connect are defined below in this file.)

use std::borrow::Cow;
use std::future::Future;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use log::{debug, info, trace};

use crate::client::dfs::DfsResolver;
use crate::error::Result;
use crate::pack::Unpack;
use crate::rpc::srvsvc::ShareInfo;
use crate::types::status::NtStatus;
use crate::types::{FileId, SessionId};
use crate::Error;
use connections::{ConnectionEntry, Connections};

/// Configuration for an SMB client connection.
///
/// `Debug` prints every field except the password, which shows as `***`.
#[derive(Clone)]
pub struct ClientConfig {
    /// Server address (host:port).
    pub addr: String,
    /// Connection timeout.
    pub timeout: Duration,
    /// Username. For a guest session, leave it empty or use `Guest` (any
    /// ASCII case): the server may then answer with a guest or anonymous
    /// session. Any other name is an account, and a guest or anonymous answer
    /// to it fails the login with [`Error::GuestSessionRefused`], since
    /// that's what a wrong password on a `map to guest = bad user` Samba, or an
    /// on-path downgrade, looks like. Some servers take the named `Guest` login
    /// but refuse an anonymous one.
    pub username: String,
    /// Password (empty for guest).
    ///
    /// **Security note:** The password is stored in memory so that the client
    /// can reconnect without asking the user again. It is not encrypted in
    /// memory. Ensure the `SmbClient` is dropped when no longer needed.
    pub password: String,
    /// Domain (empty for local).
    pub domain: String,
    /// Bring the connection back by itself when the session dies.
    ///
    /// With this on, [`SmbClient::connect`] arms the connection with a
    /// [`SessionReviver`] built from this config, so a dead session (a NAS
    /// rebooting, a Wi-Fi roam with no TCP reset, a share going briefly
    /// offline) is re-dialed, re-negotiated, and re-authenticated in place
    /// under every `Connection` clone the consumer is holding. Bounds live in
    /// [`ReconnectPolicy`]; nothing here can retry forever.
    ///
    /// **What it does NOT do is re-issue arbitrary work.** Only operations
    /// whose retry cannot change what the caller asked for are replayed
    /// (directory listings, reads, `stat`, `fs_info`). A `delete`, `rename`,
    /// or `create` that died in flight may already have taken effect on the
    /// server, so it surfaces the error and lets the caller decide — hiding
    /// that would be the library guessing about the user's data.
    ///
    /// **Security note:** the reviver keeps a copy of
    /// [`password`](Self::password) for the life of the client, for the same
    /// reason this struct does.
    pub auto_reconnect: bool,
    /// Enable LZ4 compression for SMB 3.1.1 connections.
    /// When enabled, messages are compressed if it reduces their size.
    /// Incompressible data (photos, videos) is sent uncompressed automatically.
    /// Default: true.
    pub compression: bool,
    /// Enable DFS (Distributed File System) path resolution.
    ///
    /// When `true`, operations that receive a DFS referral response
    /// (`STATUS_PATH_NOT_COVERED`) automatically resolve the referral,
    /// connect to the target server, and retry the operation.
    /// Default: true.
    pub dfs_enabled: bool,
    /// Override addresses for DFS target servers.
    ///
    /// Maps server hostnames (as they appear in DFS referrals) to
    /// `host:port` socket addresses. Useful when DFS targets use
    /// internal hostnames that the client can't resolve, or when
    /// port mapping is needed (for example, Docker test environments).
    ///
    /// Default: empty (use the server hostname from the referral
    /// with port 445).
    pub dfs_target_overrides: std::collections::HashMap<String, String>,
    /// Who decides whether this client may sign in to a server a DFS referral
    /// named, when the two aren't provably the same server.
    ///
    /// **Security:** a referral comes from the server, and following one sends
    /// these credentials wherever it says, so a malicious share could collect
    /// them. The default, [`DfsTargetPolicy::SameServerOnly`], refuses those with
    /// [`Error::DfsTargetNotAllowed`] and dials nothing.
    pub dfs_target_policy: DfsTargetPolicy,
    /// How the TCP connect is spread across the addresses
    /// [`addr`](Self::addr) resolves to.
    ///
    /// `None` uses [`ConnectOptions::with_timeout`](crate::transport::ConnectOptions::with_timeout) with
    /// [`timeout`](Self::timeout), which is what you want unless you are
    /// tuning for a name with an unusual number of addresses. See
    /// [`ConnectOptions`](crate::transport::ConnectOptions) for why a single deadline around
    /// `TcpStream::connect` is not enough.
    pub connect_options: Option<crate::transport::ConnectOptions>,
    /// How long a request may go without any sign of life from the server
    /// before it gives up with [`Error::Timeout`], or `None` to wait
    /// indefinitely. The same knob as
    /// [`Connection::set_response_timeout`], which documents the clock.
    ///
    /// The reason it is here as well: it is in force from the very first
    /// request, the NEGOTIATE inside [`SmbClient::connect`], which no setter
    /// can reach. That is what bounds a server that accepts the TCP
    /// connection and then never answers, so with [`timeout`](Self::timeout)
    /// bounding the TCP connect, a `connect` against a silent server gives up
    /// within roughly `timeout` plus this. DFS target connections inherit
    /// whatever the primary connection has at the time they are dialed, and
    /// a reconnect keeps it.
    ///
    /// Default: 30 s, sized for a NAS waking a spun-down disk.
    pub response_timeout: Option<Duration>,
    /// The session this client replaces, announced as `PreviousSessionId` in
    /// its first session setup so the server ends that session and closes
    /// what it still had open.
    ///
    /// Set it when you build a new client because an old one's connection
    /// dropped: pass the old client's [`SmbClient::session_id`]. Without it the
    /// server can't tell the new session belongs to the old one, so it keeps
    /// the old session's handles open until it notices the dead connection on
    /// its own, which after a drop with no TCP reset takes minutes. Until then
    /// a file the old session had open stays locked, and deleting or renaming
    /// it meets `STATUS_SHARING_VIOLATION`. A client that revives itself
    /// ([`auto_reconnect`](Self::auto_reconnect), [`SmbClient::reconnect`])
    /// announces its old session without this.
    ///
    /// Only the server at [`addr`](Self::addr) sees it; DFS target
    /// connections start their own sessions. The server ignores an id it
    /// doesn't know, or one a different user logged in as, and Samba ignores
    /// it for a guest login: only a named account's session is ended there
    /// (verified against Samba 4.20.6 and its source, 2026-10-20).
    ///
    /// ❌ **Only a session you've given up on.** The server ends the session
    /// whether or not it was dead, so a session another client is still using
    /// (a sibling connection to the same server, say) is cut off mid-request:
    /// on a signed session, every request it sends afterwards fails with
    /// [`Error::SignatureInvalid`], since the server's refusal comes back
    /// unsigned. Name each replaced session once, from the client that
    /// replaces it. And on Windows 8.1 and later this closes the old session's
    /// durable opens too, so leave it unset when you mean to reclaim a durable
    /// handle the old session held.
    ///
    /// Default: `None`.
    pub previous_session_id: Option<SessionId>,
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Destructured, so a new field can't be left out of the output.
        let Self {
            addr,
            timeout,
            username,
            password: _,
            domain,
            auto_reconnect,
            compression,
            dfs_enabled,
            dfs_target_overrides,
            dfs_target_policy,
            connect_options,
            response_timeout,
            previous_session_id,
        } = self;
        f.debug_struct("ClientConfig")
            .field("addr", addr)
            .field("timeout", timeout)
            .field("username", username)
            .field("password", &crate::crypto::Redacted)
            .field("domain", domain)
            .field("auto_reconnect", auto_reconnect)
            .field("compression", compression)
            .field("dfs_enabled", dfs_enabled)
            .field("dfs_target_overrides", dfs_target_overrides)
            .field("dfs_target_policy", dfs_target_policy)
            .field("connect_options", connect_options)
            .field("response_timeout", response_timeout)
            .field("previous_session_id", previous_session_id)
            .finish()
    }
}

impl ClientConfig {
    /// The connect budget this config asks for: its own
    /// [`connect_options`](Self::connect_options), or the defaults with
    /// [`timeout`](Self::timeout) as the whole-attempt budget.
    pub(crate) fn connect_options(&self) -> crate::transport::ConnectOptions {
        self.connect_options
            .clone()
            .unwrap_or_else(|| crate::transport::ConnectOptions::with_timeout(self.timeout))
    }
}

impl Default for ClientConfig {
    /// Guest access to nothing, with compression and DFS on.
    ///
    /// Exists so a field added here is not a breaking change for every caller
    /// writing a struct literal, which is all of them: fill in
    /// [`addr`](Self::addr) and whatever else you need, and spread the rest.
    ///
    /// ```
    /// # use smb2::ClientConfig;
    /// let config = ClientConfig {
    ///     addr: "nas.local:445".to_string(),
    ///     username: "david".to_string(),
    ///     auto_reconnect: true,
    ///     ..Default::default()
    /// };
    /// ```
    fn default() -> Self {
        Self {
            addr: String::new(),
            timeout: Duration::from_secs(5),
            username: String::new(),
            password: String::new(),
            domain: String::new(),
            auto_reconnect: false,
            compression: true,
            dfs_enabled: true,
            dfs_target_overrides: std::collections::HashMap::new(),
            dfs_target_policy: DfsTargetPolicy::default(),
            connect_options: None,
            response_timeout: Some(connection::RESPONSE_TIMEOUT),
            previous_session_id: None,
        }
    }
}

/// Dials and re-authenticates on a consumer's behalf when a session dies.
///
/// Holds a snapshot of the client's config rather than a back-reference to the
/// [`SmbClient`], so a revival can run from any `Connection` clone (a
/// `FileWriter` deep in a transfer, a `Watcher` on its own task) without
/// reaching back through a client nobody has a handle to.
///
/// **Security note:** it keeps the password for the life of the client, which
/// is the same trade [`SmbClient`] already makes to reconnect without
/// re-prompting.
struct ClientReviver {
    addr: String,
    connect_options: crate::transport::ConnectOptions,
    compression: bool,
    username: String,
    password: String,
    domain: String,
    /// For a DFS target: what has to allow the sign-in, every time. A reviver
    /// holds the password and dials from any connection clone, so it asks like
    /// the client's own dial does. `None` on the primary: its server is the one
    /// the caller chose.
    dfs_gate: Option<RevivalGate>,
}

/// What a DFS target's reviver asks before it dials.
struct RevivalGate {
    policy: DfsTargetPolicy,
    primary_addr: String,
    named: String,
}

impl ClientReviver {
    fn from_config(config: &ClientConfig) -> Self {
        Self::for_addr(config, config.addr.clone())
    }

    /// A reviver for a server other than the one the client was built for.
    ///
    /// A DFS target lives at its own address with its own session, so it needs
    /// its own reviver; `config.addr` would dial the namespace server back.
    /// Everything else (credentials, timeout, compression) is the client's.
    fn for_target(config: &ClientConfig, named: &str, addr: String) -> Self {
        let mut reviver = Self::for_addr(config, addr);
        reviver.dfs_gate = Some(RevivalGate {
            policy: config.dfs_target_policy.clone(),
            primary_addr: config.addr.clone(),
            named: named.to_string(),
        });
        reviver
    }

    fn for_addr(config: &ClientConfig, addr: String) -> Self {
        Self {
            dfs_gate: None,
            addr,
            connect_options: config.connect_options(),
            compression: config.compression,
            username: config.username.clone(),
            password: config.password.clone(),
            domain: config.domain.clone(),
        }
    }
}

#[async_trait::async_trait]
impl connection::SessionReviver for ClientReviver {
    async fn dial(
        &self,
    ) -> Result<(
        Box<dyn crate::transport::TransportSend>,
        Box<dyn crate::transport::TransportReceive>,
    )> {
        if let Some(gate) = &self.dfs_gate {
            gate.policy
                .check(&gate.primary_addr, &gate.named, &self.addr)
                .map_err(|sign_in| dfs_target_not_allowed(&gate.named, sign_in))?;
        }
        let transport = std::sync::Arc::new(
            crate::transport::TcpTransport::connect_with(&self.addr, self.connect_options.clone())
                .await?,
        );
        Ok((
            Box::new(std::sync::Arc::clone(&transport)),
            Box::new(transport),
        ))
    }

    async fn reauthenticate(&self, conn: &mut Connection) -> Result<()> {
        conn.set_compression_requested(self.compression);
        conn.negotiate().await?;
        // `Session::setup` publishes the new session onto the connection, so
        // an `SmbClient` that has one cached picks the new keys up.
        Session::setup(conn, &self.username, &self.password, &self.domain).await?;
        Ok(())
    }
}

/// The error for a sign-in [`DfsTargetPolicy`] refused.
fn dfs_target_not_allowed(path: &str, sign_in: DfsSignIn) -> Error {
    debug!(
        "dfs: not signing in to a different server without consent server={:?} target={:?} path={path:?}",
        sign_in.server, sign_in.target
    );
    Error::DfsTargetNotAllowed {
        path: path.to_string(),
        server: sign_in.server,
        target: sign_in.target,
    }
}

/// Turn on encryption for a share that asked for it
/// (`SMB2_SHAREFLAG_ENCRYPT_DATA`), if it is not on already.
///
/// ❌ **Every tree connect goes through here.** A share that asked to be
/// encrypted and is not is a data-confidentiality bug, not a convenience gap,
/// and a DFS target share is exactly as entitled to it as the one the caller
/// named. Falls back to AES-128-CCM when the server sent no encryption
/// negotiate context, the same fallback session-level encryption makes.
fn activate_share_encryption(conn: &mut Connection, session: &Session, tree: &Tree) {
    if !tree.encrypt_data || conn.should_encrypt() {
        return;
    }
    let (Some(enc_key), Some(dec_key)) = (&session.encryption_key, &session.decryption_key) else {
        return;
    };
    let cipher = conn
        .params()
        .and_then(|p| p.cipher)
        .unwrap_or(crate::crypto::encryption::Cipher::Aes128Ccm);
    conn.activate_encryption(enc_key.clone(), dec_key.clone(), cipher);
}

/// How many referrals one connect may follow before we call it a loop.
///
/// A real namespace resolves in one hop, and an Interlink (MS-DFSC
/// § 3.1.5.4.5) adds one more. Anything near this limit is a namespace
/// pointing at itself or a chain with no end, which is a misconfiguration on
/// the server: a knob here would only let a consumer wait longer for the same
/// answer.
const MAX_DFS_HOPS: usize = 8;

/// The host half of a `host:port` address, for the places that need a NAME
/// rather than something to dial: the UNC path in every `TREE_CONNECT` and the
/// DFS referral cache key.
///
/// ❌ Never `split(':')`. That reads `[::1]:445` as `[` and `fe80::1:445` as
/// `fe80`, and the wrong name then goes out as the server half of the UNC path.
/// Two sites derived this separately and disagreed, which on a DFS share means
/// the cache key and the tree-connect path name different servers.
///
/// **Matches what `ToSocketAddrs` will actually dial**, which is the whole
/// point of deriving it here rather than guessing: std splits a bracket-less
/// address on the LAST colon, so `fe80::1:445` is host `fe80::1` on port 445
/// even though the same text is also a valid IPv6 address in its own right.
/// A bracket-less literal with no port is genuinely ambiguous and is read as
/// carrying a port, exactly as std reads it; one cannot reach here anyway,
/// since `lookup_host` refuses a portless address ("invalid port value") and
/// the client is never built.
pub(crate) fn host_of(addr: &str) -> &str {
    // A bracketed literal says where it ends, which is what brackets are for.
    if let Some(host) = addr
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .map(|(host, _)| host)
    {
        return host;
    }
    match addr.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => host,
        _ => addr,
    }
}

/// `path` on `tree` as the UNC path a link referral is asked for and the
/// referral cache is keyed by: `\\host\share\a\b`.
fn link_unc(tree: &Tree, path: &str) -> String {
    // The referral lookup and the tree-connect path have to name the same
    // server, so this goes through the one derivation (`host_of`). Encoded,
    // not just slash-flipped: the referral lookup and the CREATE that follows
    // have to agree on where a component ends, and a `\` that came from a
    // *name* is U+F026 in both (`crate::name`).
    format!(
        "\\\\{}\\{}\\{}",
        host_of(&tree.server),
        tree.share_name,
        crate::name::encode_path(path)
    )
}

/// What [`SmbClient::follow_dfs_link`] came back with.
struct Followed<T> {
    result: Result<T>,
    /// The link, when the call was redirected through one, whether or not
    /// the call then succeeded on its target.
    moved_to: Option<Link>,
}

/// A DFS link a call followed.
struct Link {
    /// The link's target share, pooled (`Connections::link_tree`).
    tree: Tree,
    /// The link folder, as the caller's path spells it (`data`, `a/data`).
    link_folder: Vec<String>,
    /// The same folder on `tree`: empty when the link points at its target
    /// share's root, `sub` for a link to `\\fs02\files\sub`.
    target_folder: Vec<String>,
}

impl Link {
    fn new(tree: Tree, caller_path: &str, remaining: &str, link_depth: usize) -> Self {
        let caller = components(caller_path);
        let covers = link_depth.min(caller.len());
        let remaining = components(remaining);
        let target_prefix = remaining.len().saturating_sub(caller.len() - covers);
        Link {
            tree,
            link_folder: caller[..covers].iter().map(|c| c.to_string()).collect(),
            target_folder: remaining[..target_prefix]
                .iter()
                .map(|c| c.to_string())
                .collect(),
        }
    }

    /// `target_path`, a path relative to the link's target share, in the
    /// caller's terms: relative to the caller's own share, link folder first.
    fn caller_path(&self, target_path: &str) -> String {
        self.link_folder
            .iter()
            .map(String::as_str)
            .chain(
                components(target_path)
                    .into_iter()
                    .skip(self.target_folder.len()),
            )
            .collect::<Vec<_>>()
            .join("/")
    }

    /// `caller_path`, a path on the caller's share, as a path on the link's
    /// target, when it sits inside the link folder. `None` for a path outside
    /// the link, or the link folder itself, which has no name on the target.
    ///
    /// The folder matches case-insensitively, as DFS paths do (MS-DFSC
    /// § 3.1.4.1).
    fn target_path(&self, caller_path: &str) -> Option<String> {
        let caller = components(caller_path);
        let inside = caller.len() > self.link_folder.len()
            && self
                .link_folder
                .iter()
                .zip(&caller)
                .all(|(link, c)| link.to_lowercase() == c.to_lowercase());
        inside.then(|| {
            self.target_folder
                .iter()
                .map(String::as_str)
                .chain(caller[self.link_folder.len()..].iter().copied())
                .collect::<Vec<_>>()
                .join("/")
        })
    }

    /// The link folder in the caller's terms, `/`-joined.
    fn link_path(&self) -> String {
        self.link_folder.join("/")
    }
}

/// A caller path's components (`/` is the only separator a caller writes).
fn components(path: &str) -> Vec<&str> {
    path.split('/').filter(|c| !c.is_empty()).collect()
}

/// High-level SMB2 client with reconnection support.
///
/// Wraps a [`Connection`] + [`Session`] and provides methods for connecting
/// to shares, listing shares, and reconnecting after network failures.
///
/// **Security note:** This struct stores the password in memory so it can
/// reconnect without asking the user again. The password is not encrypted.
/// Drop the `SmbClient` when no longer needed.
pub struct SmbClient {
    config: ClientConfig,
    /// The primary connection and the DFS pool. ❌ A method holding a `Tree`
    /// reaches its connection through `connections.for_tree`, never the
    /// primary directly (`connections.rs`).
    connections: Connections,
    /// The session as of the last authentication. Behind an `Arc` because a
    /// revival can establish a new one behind this client's back; every
    /// `&mut self` path that reads the session keys refreshes it first.
    session: std::sync::Arc<Session>,
    /// DFS referral resolver with TTL-based cache.
    dfs_resolver: DfsResolver,
    /// Client-level counter: how many times `reconnect()` ran. Survives
    /// each reconnect (per-connection counters do not).
    reconnects: AtomicU64,
}

impl SmbClient {
    /// Connect to an SMB server and authenticate.
    ///
    /// Performs TCP connect, negotiate, and session setup in one call.
    pub async fn connect(config: ClientConfig) -> Result<Self> {
        debug!("smb_client: connecting addr={:?}", config.addr);

        let mut conn = Connection::connect_with(&config.addr, config.connect_options()).await?;
        conn.set_compression_requested(config.compression);
        // Before NEGOTIATE, so the handshake itself is bounded by it.
        conn.set_response_timeout(config.response_timeout);
        conn.negotiate().await?;
        if let Some(previous) = config.previous_session_id {
            conn.set_previous_session_id(previous);
        }

        let session = Session::setup(
            &mut conn,
            &config.username,
            &config.password,
            &config.domain,
        )
        .await?;

        debug!(
            "smb_client: connected and authenticated, session_id={}, compression={}",
            session.session_id,
            conn.compression_enabled()
        );

        let primary_server = config.addr.clone();
        if config.auto_reconnect {
            conn.set_reviver(Some(std::sync::Arc::new(ClientReviver::from_config(
                &config,
            ))));
        }

        Ok(SmbClient {
            config,
            connections: Connections::new(conn, primary_server),
            session: std::sync::Arc::new(session),
            dfs_resolver: DfsResolver::new(),
            reconnects: AtomicU64::new(0),
        })
    }

    /// Connect using an existing connection and session (for testing).
    #[cfg(test)]
    pub(crate) fn from_parts(config: ClientConfig, conn: Connection, session: Session) -> Self {
        let primary_server = config.addr.clone();
        SmbClient {
            config,
            connections: Connections::new(conn, primary_server),
            session: std::sync::Arc::new(session),
            dfs_resolver: DfsResolver::new(),
            reconnects: AtomicU64::new(0),
        }
    }

    /// Adopt a session established on the connection behind this client's
    /// back, which is what a revival does.
    ///
    /// ❌ Skipping this is not cosmetic: [`connect_share`](Self::connect_share)
    /// activates share encryption from these keys, and the previous session's
    /// keys decrypt nothing — every frame afterwards fails.
    fn refresh_session(&mut self) {
        if let Some(current) = self.connections.primary().current_session() {
            if current.session_id != self.session.session_id {
                debug!(
                    "smb_client: adopting session {} established by a reconnect \
                     (was {})",
                    current.session_id, self.session.session_id
                );
                self.session = current;
            }
        }
    }

    /// List available shares on the server.
    ///
    /// Connects to the IPC$ share, performs an RPC exchange via the srvsvc
    /// named pipe, and returns only disk shares (excluding admin shares
    /// ending with `$`).
    pub async fn list_shares(&mut self) -> Result<Vec<ShareInfo>> {
        shares::list_shares(self.connections.primary_mut()).await
    }

    /// Connect to a share on the server.
    ///
    /// If the share requires encryption (`SMB2_SHAREFLAG_ENCRYPT_DATA`)
    /// and encryption is not already active, encryption is activated
    /// using the session's keys.
    ///
    /// # DFS namespace roots
    ///
    /// `share_name` may be a **DFS namespace** rather than a share on this
    /// server — `\\corp.example.com\projects`, where `corp.example.com` is a domain name and
    /// `projects` is the namespace. There is no such share to tree-connect, so the
    /// server refuses with `STATUS_BAD_NETWORK_NAME`; this method then asks it
    /// for a root referral and connects the target the referral names, on
    /// whichever server that turns out to be. The returned `Tree` carries a
    /// [`DfsOrigin`] recording both names, so a consumer can keep showing the
    /// path the person asked for.
    ///
    /// A cached namespace skips straight to the target, so the wasted
    /// `TREE_CONNECT` is paid once per TTL rather than once per connect.
    ///
    /// **A mistyped share name is still a mistyped share name.** The referral
    /// only happens when the server advertised `SMB2_GLOBAL_CAP_DFS`, and if
    /// it fails, comes back empty, or names nothing reachable-looking, the
    /// caller gets the original `STATUS_BAD_NETWORK_NAME` back rather than
    /// something about DFS. The one case that reports differently is a real
    /// namespace whose storage is all down:
    /// [`Error::DfsNoReachableTarget`].
    pub async fn connect_share(&mut self, share_name: &str) -> Result<Tree> {
        self.refresh_session();

        // § 3.1.4.1 step 2: the cache first, so a known namespace never pays
        // the refused TreeConnect again inside its TTL. Terminal on purpose —
        // falling back to a plain tree connect afterwards would resolve the
        // namespace twice and double the wait on the one path where the wait
        // is long (every target down). An entry whose TTL has run out reports
        // a miss, so a namespace that has genuinely become an ordinary share
        // is found again then.
        let requested = self.unc_for(share_name);
        if self.config.dfs_enabled && self.dfs_resolver.resolve_from_cache(&requested).is_some() {
            trace!("dfs: known namespace, going straight to its targets namespace={requested:?}");
            return self.connect_namespace(&requested).await;
        }

        // § 3.1.4.1 step 8 for the ordinary world: tree-connect, and on
        // success we are done at zero extra cost.
        let refusal = match Tree::connect(self.connections.primary_mut(), share_name).await {
            Ok(mut tree) => {
                tree.server = self.connections.primary_server().to_string();
                activate_share_encryption(self.connections.primary_mut(), &self.session, &tree);
                return Ok(tree);
            }
            Err(err) => err,
        };

        if !self.looks_like_a_namespace_root(&refusal) {
            return Err(refusal);
        }

        debug!(
            "dfs: not a share on the server, and the server speaks DFS; asking it for a root \
             referral share={share_name:?} server={:?}",
            self.connections.primary_server()
        );
        match self.connect_namespace(&requested).await {
            Ok(tree) => Ok(tree),
            // ❌ A failed referral must not replace the original error. The
            // overwhelmingly common reason for STATUS_BAD_NETWORK_NAME is a
            // typo in a share name, and telling that person about DFS is
            // worse than telling them nothing. Only the one outcome that is
            // genuinely about DFS survives.
            Err(
                err @ (Error::DfsNoReachableTarget { .. }
                | Error::DfsTooManyReferrals { .. }
                | Error::DfsTargetNotAllowed { .. }),
            ) => Err(err),
            Err(err) => {
                debug!(
                    "dfs: no namespace behind the share, reporting the refusal share={share_name:?}: {err}"
                );
                Err(refusal)
            }
        }
    }

    /// The UNC path for a share on the primary server, as a referral names it.
    fn unc_for(&self, share_name: &str) -> String {
        format!(
            r"\\{}\{}",
            host_of(self.connections.primary_server()),
            share_name
        )
    }

    /// Whether a refused `TREE_CONNECT` might be a DFS namespace root rather
    /// than a share that is simply not there.
    ///
    /// MS-SMB2 § 3.3.5.7 makes `STATUS_BAD_NETWORK_NAME` the required answer
    /// for a name the server has no share for, which is what a namespace root
    /// is from the server's point of view. Gating on the negotiated
    /// `SMB2_GLOBAL_CAP_DFS` keeps a plain NAS at zero extra round-trips when
    /// someone mistypes a share name; a server that does advertise DFS pays
    /// one extra frame on an error path.
    ///
    /// `Error::ShareRedirected` deliberately does not reach here:
    /// `Tree::connect` separates cluster redirection out, and it shares only
    /// the status code with this.
    fn looks_like_a_namespace_root(&self, err: &Error) -> bool {
        self.config.dfs_enabled
            && matches!(
                err,
                Error::Protocol {
                    status: NtStatus::BAD_NETWORK_NAME,
                    command: crate::types::Command::TreeConnect,
                }
            )
            && self.connections.primary().params().is_some_and(|p| {
                p.capabilities
                    .contains(crate::types::flags::Capabilities::DFS)
            })
    }

    /// Resolve `requested` as a DFS namespace and connect the first target
    /// that answers.
    ///
    /// Follows Interlinks (MS-DFSC § 3.1.5.4.5: the target is a path in
    /// another namespace, so it is re-resolved rather than connected) up to
    /// [`MAX_DFS_HOPS`], which is what stops a namespace that points at
    /// itself from resolving forever.
    async fn connect_namespace(&mut self, requested: &str) -> Result<Tree> {
        let mut path = requested.to_string();

        for _ in 0..MAX_DFS_HOPS {
            let targets = self.root_referral(&path, requested).await?;

            // The chain ended without naming anywhere to go. ❌ This has to
            // leave by its own exit: sharing one with the hop limit below
            // means telling the caller we exhausted eight hops when the chain
            // simply ran out after two. `DfsResolver::resolve` reports an
            // error rather than an empty list today, so this is a guard on its
            // contract rather than a path anything takes.
            if targets.is_empty() {
                return Err(Error::invalid_data(if path == requested {
                    format!("DFS referral for {requested} named no targets")
                } else {
                    format!(
                        "DFS referral for {requested} followed an interlink to {path}, \
                         which named no targets"
                    )
                }));
            }

            // An Interlink means the whole response is a path in another
            // namespace, so there is nothing here to connect to.
            if let Some(first) = targets.first().filter(|t| t.interlink) {
                path = format!(r"\\{}\{}", first.server, first.share);
                trace!("dfs: interlink, re-resolving namespace={requested:?} target={path:?}");
                continue;
            }

            // Past here the hop is decided: every exit below returns, so
            // nothing after the loop has to work out how we got there.
            let target_count = targets.len();
            let mut last_error: Option<Error> = None;
            let mut refused: Option<Error> = None;

            for target in &targets {
                // A root target is `\\server\share` and nothing more
                // (MS-DFSC § 2.2.1.6). A `Tree` cannot express a path suffix,
                // so a target carrying one is skipped rather than silently
                // dropping the suffix and opening the wrong directory.
                if !target.remaining_path.is_empty() {
                    debug!(
                        "dfs: skipping a root target that carries a path suffix, which a root \
                         target cannot namespace={requested:?} server={:?} share={:?} suffix={:?}",
                        target.server, target.share, target.remaining_path
                    );
                    continue;
                }

                let target_addr = self.target_addr(target);
                match self
                    .connect_target(&target.server, &target_addr, &target.share, requested)
                    .await
                {
                    Ok(mut tree) => {
                        self.dfs_resolver.note_target_worked(target);
                        tree.dfs_origin = Some(DfsOrigin {
                            requested: requested.to_string(),
                            target: format!(r"\\{}\{}", target.server, target.share),
                        });
                        info!(
                            "dfs: namespace resolved namespace={requested:?} server={:?} share={:?}",
                            target.server, target.share
                        );
                        return Ok(tree);
                    }
                    Err(err @ Error::DfsTargetNotAllowed { .. }) => {
                        refused.get_or_insert(err);
                    }
                    Err(err) => {
                        debug!(
                            "dfs: target refused namespace={requested:?} addr={target_addr:?}: {err}"
                        );
                        last_error = Some(err);
                    }
                }
            }

            // A target it may not sign in to outranks one that didn't answer: only
            // the first is something the caller can change.
            if let Some(refused) = refused {
                return Err(refused);
            }
            return match last_error {
                Some(source) => Err(Error::DfsNoReachableTarget {
                    namespace: requested.to_string(),
                    target_count,
                    source: Box::new(source),
                }),
                // Every target was skipped as unusable rather than failing,
                // so there is no connectivity story to tell.
                None => Err(Error::invalid_data(format!(
                    "DFS referral for {requested} named {target_count} target(s), \
                     none of them a share this client can connect to"
                ))),
            };
        }

        // The only way out of that loop is the Interlink `continue`, once per
        // hop, so reaching here means exactly that and nothing else.
        Err(Error::DfsTooManyReferrals {
            namespace: requested.to_string(),
            hops: MAX_DFS_HOPS,
        })
    }

    /// Ask the server that owns `path` for a referral, resolving from cache
    /// when it is already known.
    async fn root_referral(
        &mut self,
        path: &str,
        requested: &str,
    ) -> Result<Vec<dfs::ResolvedPath>> {
        // The referral goes to the server naming the path, which after an
        // Interlink is not the one the caller started on.
        let host = path.trim_start_matches('\\');
        let host = host.split('\\').next().unwrap_or(host);
        let addr = self.dfs_addr_for_host(host);

        self.ensure_connection(host, &addr, requested).await?;
        let conn = self.connections.for_addr(&addr)?;
        self.dfs_resolver.resolve(conn, path).await
    }

    /// Where to actually dial for a referral target, honouring
    /// [`ClientConfig::dfs_target_overrides`].
    fn target_addr(&self, target: &dfs::ResolvedPath) -> String {
        self.config
            .dfs_target_overrides
            .get(&target.server)
            .cloned()
            .unwrap_or_else(|| format!("{}:{}", target.server, target.port))
    }

    /// The same mapping for a bare host name, which is what an Interlink
    /// hands back.
    fn dfs_addr_for_host(&self, host: &str) -> String {
        self.config
            .dfs_target_overrides
            .get(host)
            .cloned()
            .unwrap_or_else(|| {
                let primary = self.connections.primary_server();
                if host == primary || primary.starts_with(&format!("{host}:")) {
                    primary.to_string()
                } else {
                    format!("{host}:445")
                }
            })
    }

    /// Connect (pooling the connection) and tree-connect one referral target.
    async fn connect_target(
        &mut self,
        named: &str,
        target_addr: &str,
        share: &str,
        asked: &str,
    ) -> Result<Tree> {
        self.ensure_connection(named, target_addr, asked).await?;
        self.ensure_tree(target_addr, share).await
    }

    /// Reconnect now, whether or not the connection has noticed it is dead.
    ///
    /// Dials a fresh socket, renegotiates, and re-authenticates with the
    /// stored credentials, **in place under the existing connection**: every
    /// `Connection` clone the consumer is holding (a `FileWriter` mid-upload,
    /// a `Watcher`, a pipelined task) stays usable. All previous tree
    /// connections and file handles are invalidated regardless — they belong
    /// to a session that no longer exists — so the caller must re-do
    /// [`connect_share`](Self::connect_share) for any shares it still needs.
    ///
    /// Bounded by [`ReconnectPolicy`]; on failure the
    /// connection is left unambiguously dead and the error is
    /// [`Error::ReconnectFailed`].
    pub async fn reconnect(&mut self) -> Result<()> {
        debug!("smb_client: reconnecting addr={:?}", self.config.addr);
        self.reconnects.fetch_add(1, Ordering::Relaxed);

        // An explicit reconnect works even when `auto_reconnect` is off: the
        // caller is asking for exactly this, and everything needed to do it is
        // already in the config.
        let primary = self.connections.primary_mut();
        if !primary.can_reconnect() {
            primary.set_reviver(Some(std::sync::Arc::new(ClientReviver::from_config(
                &self.config,
            ))));
        }
        // Say so first: `reconnect_if_needed` is a no-op on a connection that
        // still looks alive, and a caller reaching for this has decided
        // otherwise.
        primary.mark_dead();
        primary.reconnect_if_needed().await?;
        self.refresh_session();

        self.connections.clear_extras();

        debug!(
            "smb_client: reconnected, new session_id={}",
            self.session.session_id
        );
        Ok(())
    }

    /// Whether the connection is currently torn down.
    pub fn is_disconnected(&self) -> bool {
        self.connections.primary().is_disconnected()
    }

    /// Be told about every reconnect as it happens. See
    /// [`Connection::on_reconnect`].
    pub fn on_reconnect(&self, observer: Option<connection::ReconnectObserver>) {
        self.connections.primary().on_reconnect(observer);
    }

    /// Replace the bounds on an automatic reconnect. See
    /// [`ReconnectPolicy`].
    pub fn set_reconnect_policy(&self, policy: connection::ReconnectPolicy) {
        self.connections.primary().set_reconnect_policy(policy);
    }

    /// Get the negotiated parameters, or `None` before NEGOTIATE has run.
    ///
    /// Owned rather than borrowed: the parameters are replaced whenever the
    /// connection is revived on a fresh socket. Every field is a scalar, so
    /// the copy costs nothing.
    pub fn params(&self) -> Option<NegotiatedParams> {
        self.connections.primary().params()
    }

    /// Get the session info.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The id of the session this client's connection is on now.
    ///
    /// Remember it while the client works: when its connection drops and you
    /// build a new client in its place, pass it as
    /// [`ClientConfig::previous_session_id`] so the server closes what this
    /// session had open. Unlike [`session`](Self::session), it follows a
    /// session a reconnect established behind this client's back.
    pub fn session_id(&self) -> SessionId {
        self.connections.primary().session_id()
    }

    /// Get the client config.
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Current number of available credits.
    pub fn credits(&self) -> u16 {
        self.connections.primary().credits()
    }

    /// Estimated round-trip time from the negotiate exchange.
    pub fn estimated_rtt(&self) -> Option<Duration> {
        self.connections.primary().estimated_rtt()
    }

    /// Capture a tree of diagnostics: client config, primary + DFS-extra
    /// connections, the session on each connection, per-connection
    /// counters, the DFS referral cache, and client-level counters.
    ///
    /// See [`crate::client::diagnostics`] for the consistency model. In
    /// short: eventually consistent, snapshot survives connection
    /// teardown, per-connection counters reset on
    /// [`Self::reconnect`], client-level counters survive.
    pub fn diagnostics(&self) -> crate::client::diagnostics::Diagnostics {
        use crate::client::diagnostics::{
            ClientInfo, ClientMetricsSnapshot, Diagnostics, SessionDiagnostics,
        };

        let (cache_hits, referrals_resolved) = self.dfs_resolver.counters();
        let client = ClientInfo {
            primary_server: self.connections.primary_server().to_string(),
            timeout: self.config.timeout,
            auto_reconnect: self.config.auto_reconnect,
            dfs_enabled: self.config.dfs_enabled,
            metrics: ClientMetricsSnapshot {
                reconnects: self.reconnects.load(Ordering::Relaxed),
                dfs_referrals_resolved: referrals_resolved,
                dfs_cache_hits: cache_hits,
            },
        };

        let session_for = |s: &Session| SessionDiagnostics {
            session_id: s.session_id,
            should_sign: s.should_sign,
            should_encrypt: s.should_encrypt,
            signing_algorithm: s.signing_algorithm,
        };

        let mut primary = self.connections.primary().diagnostics();
        primary.session = Some(session_for(&self.session));

        let extra_connections = self
            .connections
            .extras()
            .map(|entry| {
                let mut d = entry.conn.diagnostics();
                d.session = Some(session_for(&entry.session));
                d
            })
            .collect();

        Diagnostics {
            client,
            primary,
            extra_connections,
            dfs_cache: self.dfs_resolver.cache_entries(),
        }
    }

    /// The primary connection, for reading its state without `&mut`.
    ///
    /// What a watchdog or a status display polls:
    /// [`Connection::liveness`] and [`Connection::inbound`] take `&self`. A
    /// `clone()` of it is cheap and shares everything, so a consumer that
    /// keeps the client behind a lock can clone one out once and poll that.
    /// DFS cross-server connections are separate; see
    /// [`diagnostics`](Self::diagnostics) for all of them.
    pub fn connection(&self) -> &Connection {
        self.connections.primary()
    }

    /// Get a mutable reference to the underlying connection.
    ///
    /// Needed when using [`Tree`] methods directly, since they require
    /// `&mut Connection`. For most use cases, prefer the convenience methods
    /// on `SmbClient` (like [`list_directory`](Self::list_directory)) instead.
    pub fn connection_mut(&mut self) -> &mut Connection {
        self.connections.primary_mut()
    }

    /// The connection `tree` lives on, for calling [`Tree`]'s own methods
    /// without holding the client: the primary for a tree on the server this
    /// client connected to, the pooled one for a DFS target's tree (such as
    /// [`DfsLink::tree`]). A clone is cheap and shares everything.
    ///
    /// [`Error::Disconnected`] when that connection is gone: a reconnect
    /// drops every DFS connection, so `connect_share` (or
    /// [`resolve_dfs_link`](Self::resolve_dfs_link)) again.
    pub fn connection_for(&self, tree: &Tree) -> Result<Connection> {
        self.connections.for_tree_ref(tree).cloned()
    }

    /// Where `path` on `tree` really lives when it sits behind a DFS link: the
    /// link's target share (connected and pooled), the path on it, and the
    /// link folder. `None` when it doesn't sit behind one.
    ///
    /// Every `SmbClient` call already follows links, so this is for a
    /// consumer that calls [`Tree`]'s own methods, on its own task or its own
    /// connection: it can send them to the target directly rather than to
    /// `tree`, where each would be refused with `STATUS_PATH_NOT_COVERED`
    /// first. `tree` itself never moves.
    ///
    /// What it costs:
    /// - **Nothing on the wire** when DFS is off ([`ClientConfig::dfs_enabled`]),
    ///   `tree` isn't a DFS share ([`Tree::is_dfs`]), or `path` is the share's
    ///   root.
    /// - **Nothing on the wire** when the referral cache already knows the
    ///   link (any earlier call through it, or this one, put it there; it
    ///   lasts for the referral's TTL). The target's tree connect is pooled,
    ///   so it's paid once per target share.
    /// - **One referral round trip** otherwise. A path outside every link
    ///   isn't remembered, so ask once per folder rather than per file:
    ///   everything under [`DfsLink::link_path`] lives on the same target.
    ///
    /// # Errors
    ///
    /// [`Error::DfsTargetUnreachable`] when the link is real and none of its
    /// targets answered; otherwise whatever stopped the referral exchange.
    /// A server that refuses the referral is saying there's no link there,
    /// which is `Ok(None)`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(client: &mut smb2::SmbClient, share: &smb2::Tree) -> Result<(), smb2::Error> {
    /// let (tree, path, mut conn) = match client.resolve_dfs_link(share, "data/report.pdf").await? {
    ///     Some(link) => {
    ///         let conn = client.connection_for(&link.tree)?;
    ///         (link.tree, link.path, conn)
    ///     }
    ///     None => (share.clone(), "data/report.pdf".to_string(), client.connection_for(share)?),
    /// };
    /// // The client is free again; the stat goes straight to where the file is.
    /// let info = tree.stat(&mut conn, &path).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn resolve_dfs_link(&mut self, tree: &Tree, path: &str) -> Result<Option<DfsLink>> {
        if !self.config.dfs_enabled || !tree.is_dfs || components(path).is_empty() {
            return Ok(None);
        }
        let found = match self.cached_link(tree, path).await {
            Some(found) => found?,
            None => {
                let unc_path = link_unc(tree, path);
                let conn = self.connections.for_tree(tree)?;
                let Some(resolved_list) = self.dfs_resolver.resolve_link(conn, &unc_path).await?
                else {
                    return Ok(None);
                };
                self.connect_link(&resolved_list, path).await?
            }
        };
        let (link, remaining) = found;
        Ok(Some(DfsLink {
            link_path: link.link_path(),
            tree: link.tree,
            path: remaining,
        }))
    }

    // ── DFS helpers ───────────────────────────────────────────────────

    /// Run `op` on `tree` and `path`, and when the server answers that `path`
    /// sits behind a DFS link, follow the link once and run `op` again on the
    /// link's target share with the path the referral leaves.
    ///
    /// The one place every path-taking call follows links. Once only: a
    /// target answering `STATUS_PATH_NOT_COVERED` in turn is the caller's
    /// answer. ❌ A referral that can't be had never replaces the server's
    /// answer (see `client/CLAUDE.md` § DFS); the one exception is
    /// [`Error::DfsTargetUnreachable`], where the link is real and its storage
    /// isn't reachable.
    ///
    /// `op` gets clones of the connection and tree (both cheap) and an owned
    /// path, so its future borrows nothing from this client. ❌ Don't make it
    /// an `AsyncFnMut` over borrowed arguments: rustc can't prove such a
    /// future `Send` (the higher-ranked lifetimes defeat it), and consumers
    /// spawn these calls on multi-threaded runtimes.
    ///
    /// Only the call's CREATE may run in `op` when a retry would repeat a side
    /// effect (a progress callback, a pulled source chunk): the trigger is a
    /// refused CREATE (`is_dfs_link`), so running the rest afterwards, on the
    /// tree this returns, makes a redirect before any of it certain.
    ///
    /// A path the referral cache already places behind a link goes straight
    /// to the link's target (MS-DFSC § 3.1.4.1), the same place the refusal
    /// would have sent it, minus the refused round trip. A cold cache or an
    /// expired entry takes the refusal path.
    async fn follow_dfs_link<T, F, Fut>(
        &mut self,
        tree: &Tree,
        path: &str,
        op: &mut F,
    ) -> Followed<T>
    where
        F: FnMut(Connection, Tree, String) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        match self.cached_link(tree, path).await {
            Some(Ok((link, remaining))) => {
                let result = match self.connections.for_tree(&link.tree) {
                    Ok(conn) => op(conn.clone(), link.tree.clone(), remaining).await,
                    Err(e) => Err(e),
                };
                return Followed {
                    result,
                    moved_to: Some(link),
                };
            }
            Some(Err(e)) => {
                return Followed {
                    result: Err(e),
                    moved_to: None,
                }
            }
            None => {}
        }

        let first = match self.connections.for_tree(tree) {
            Ok(conn) => op(conn.clone(), tree.clone(), path.to_string()).await,
            Err(e) => Err(e),
        };
        let original = match first {
            Err(e) if self.is_dfs_link(&e) => e,
            result => {
                return Followed {
                    result,
                    moved_to: None,
                }
            }
        };

        let (moved_to, remaining) = match self.redirect(tree, path).await {
            Ok(redirected) => redirected,
            Err(e @ (Error::DfsTargetUnreachable { .. } | Error::DfsTargetNotAllowed { .. })) => {
                return Followed {
                    result: Err(e),
                    moved_to: None,
                }
            }
            Err(e) => {
                debug!(
                    "dfs: couldn't follow the link, keeping the server's answer path={path:?}: {e}"
                );
                return Followed {
                    result: Err(original),
                    moved_to: None,
                };
            }
        };
        let result = match self.connections.for_tree(&moved_to.tree) {
            Ok(conn) => op(conn.clone(), moved_to.tree.clone(), remaining).await,
            Err(e) => Err(e),
        };
        Followed {
            result,
            moved_to: Some(moved_to),
        }
    }

    /// [`follow_dfs_link`](Self::follow_dfs_link) for every path-taking call.
    /// Hands back the link's target tree when the call was redirected, for a
    /// handle to keep; ❌ never for the caller's own tree, which stays on its
    /// share so the next path on it means what the caller meant (see
    /// `client/CLAUDE.md` § DFS).
    async fn beside_tree<T, F, Fut>(
        &mut self,
        tree: &Tree,
        path: &str,
        mut op: F,
    ) -> Result<(T, Option<Link>)>
    where
        F: FnMut(Connection, Tree, String) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let followed = self.follow_dfs_link(tree, path, &mut op).await;
        match followed.result {
            Ok(value) => Ok((value, followed.moved_to)),
            Err(e) => Err(self.link_loss(tree, path, followed.moved_to.as_ref(), e)),
        }
    }

    /// [`beside_tree`](Self::beside_tree) for the calls that also run again after
    /// reviving a dead session: the reads, listings, and `stat`, whose retry
    /// can't change what was asked for (see [`ClientConfig::auto_reconnect`]).
    ///
    /// The one reason a convenience method still takes `&mut Tree`: a revival
    /// gives the share a new tree id on the new session, and `recover_tree`
    /// writes it into the caller's tree. A call that went through a DFS link
    /// isn't replayed, since its session was the target's, not the tree's.
    async fn replaying<T, F, Fut>(
        &mut self,
        tree: &mut Tree,
        path: &str,
        mut op: F,
    ) -> Result<(T, Option<Link>)>
    where
        F: FnMut(Connection, Tree, String) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let followed = self.follow_dfs_link(tree, path, &mut op).await;
        match followed.result {
            Err(e) if followed.moved_to.is_none() && self.session_is_gone(tree, &e) => {
                if let Err(e) = self.recover_tree(tree).await {
                    return Err(self.link_loss(tree, path, None, e));
                }
                let conn = self.connections.for_tree(tree)?.clone();
                match op(conn, tree.clone(), path.to_string()).await {
                    Ok(value) => Ok((value, None)),
                    Err(e) => Err(self.link_loss(tree, path, None, e)),
                }
            }
            Err(e) => Err(self.link_loss(tree, path, followed.moved_to.as_ref(), e)),
            Ok(value) => Ok((value, followed.moved_to)),
        }
    }

    /// Whether `err` is a server saying `path` sits behind a DFS link.
    ///
    /// ❌ **A refused CREATE only.** It's the one request that carries a path,
    /// so it's where a link is reported, and it's what makes the retry safe
    /// for the streamed calls: they all open before they read their source.
    fn is_dfs_link(&self, err: &Error) -> bool {
        self.config.dfs_enabled
            && matches!(
                err,
                Error::Protocol {
                    status: NtStatus::PATH_NOT_COVERED,
                    command: crate::types::Command::Create,
                }
            )
    }

    /// Resolve the DFS link covering `original_path` on `tree`, connecting the
    /// target server (pooling the connection) and share.
    ///
    /// Returns the link (its target tree, and how it maps paths) and the path
    /// the referral leaves on the target. Tries
    /// each target in turn, and says [`Error::DfsTargetUnreachable`] when the
    /// referral named targets and none of them answered.
    async fn redirect(&mut self, tree: &Tree, original_path: &str) -> Result<(Link, String)> {
        let unc_path = link_unc(tree, original_path);
        debug!("dfs: resolving path={unc_path:?}");

        // Resolve the referral (uses cache or IOCTL).
        let conn = self.connections.for_tree(tree)?;
        let resolved_list = self
            .dfs_resolver
            .resolve_link(conn, &unc_path)
            .await?
            .ok_or_else(|| Error::invalid_data("DFS: the referral names no link"))?;
        self.connect_link(&resolved_list, original_path).await
    }

    /// [`redirect`](Self::redirect) from the referral cache alone: `None`
    /// when the cache doesn't place `path` behind a link (or DFS is off),
    /// and nothing goes on the wire for it.
    async fn cached_link(&mut self, tree: &Tree, path: &str) -> Option<Result<(Link, String)>> {
        if !self.config.dfs_enabled {
            return None;
        }
        let unc_path = link_unc(tree, path);
        let resolved_list = self.dfs_resolver.link_from_cache(&unc_path)?;
        Some(self.connect_link(&resolved_list, path).await)
    }

    /// Connect the first of a link's targets that answers (multi-target
    /// failover), and the path the referral leaves on it.
    async fn connect_link(
        &mut self,
        resolved_list: &[dfs::ResolvedPath],
        original_path: &str,
    ) -> Result<(Link, String)> {
        let mut last_error = None;
        let mut share_connection_lost = None;
        let mut refused = None;
        for resolved in resolved_list {
            let target_addr = self.target_addr(resolved);
            match self
                .link_target_tree(
                    &resolved.server,
                    &target_addr,
                    &resolved.share,
                    original_path,
                )
                .await
            {
                Ok(new_tree) => {
                    // Remember which target worked, so a failover survives
                    // the next lookup instead of re-walking the dead one.
                    self.dfs_resolver.note_target_worked(resolved);
                    // Back into caller-path form: the retry goes through the
                    // ordinary `Tree` methods, which encode what they're given.
                    let remaining = crate::name::decode_path(&resolved.remaining_path);
                    let link =
                        Link::new(new_tree, original_path, &remaining, resolved.link_depth());
                    return Ok((link, remaining));
                }
                Err(e @ Error::DfsTargetNotAllowed { .. }) => {
                    refused.get_or_insert(e);
                }
                Err(e) => {
                    debug!(
                        "dfs: target refused server={:?} share={:?} addr={:?}: {}",
                        resolved.server, resolved.share, target_addr, e
                    );
                    // A target on the share's own server, with that connection
                    // gone, is the share's loss, and the caller has to hear it.
                    if self.connections.is_primary(&target_addr)
                        && self.connections.primary().is_disconnected()
                    {
                        share_connection_lost = Some(e);
                    } else {
                        last_error = Some(e);
                    }
                }
            }
        }

        if let Some(lost) = share_connection_lost {
            return Err(lost);
        }
        // A target it may not sign in to outranks one that didn't answer: only the
        // first is something the caller can change.
        if let Some(refused) = refused {
            return Err(refused);
        }
        match last_error {
            Some(source) => Err(Error::DfsTargetUnreachable {
                path: original_path.to_string(),
                target_count: resolved_list.len(),
                source: Box::new(source),
            }),
            None => Err(Error::invalid_data("DFS: no targets in referral")),
        }
    }

    /// What a call's failure says once its target is known: when `err` is a
    /// lost connection and the tree it ran on is a link's target (`moved_to`
    /// when it was redirected, else the tree it was given), that's the link's
    /// loss, and the share's own connection is fine. The pool lets the dead
    /// connection go, so the next call through the link dials afresh.
    fn link_loss(&mut self, tree: &Tree, path: &str, moved_to: Option<&Link>, err: Error) -> Error {
        use crate::ErrorKind;
        if !matches!(
            err.kind(),
            ErrorKind::ConnectionLost | ErrorKind::SessionExpired
        ) {
            return err;
        }
        let target = moved_to.map_or(tree, |link| &link.tree);
        if !self.connections.is_link_tree(target) {
            return err;
        }
        debug!(
            "dfs: a link's target connection is lost server={:?} share={:?} path={path:?}: {err}",
            target.server, target.share_name
        );
        let server = target.server.clone();
        self.connections.forget_lost_target(&server);
        Error::DfsTargetUnreachable {
            path: path.to_string(),
            target_count: 1,
            source: Box::new(err),
        }
    }

    /// A link's target share, connected once and pooled
    /// (`Connections::link_tree`).
    ///
    /// Asks [`DfsTargetPolicy`] ahead of the pool too, so a consent taken back
    /// stops the link on its next call.
    async fn link_target_tree(
        &mut self,
        named: &str,
        target_addr: &str,
        share: &str,
        asked: &str,
    ) -> Result<Tree> {
        self.check_dfs_target(named, target_addr, asked)?;
        if let Some(tree) = self.connections.link_tree(target_addr, share) {
            return Ok(tree);
        }
        let tree = self
            .connect_target(named, target_addr, share, asked)
            .await?;
        self.connections.remember_link_tree(&tree);
        Ok(tree)
    }

    /// Whether [`DfsTargetPolicy`] allows dialing `target_addr`, which a referral
    /// named `named`, on behalf of a call about `asked`. Running on the primary
    /// connection dials nothing, so it needs no answer.
    fn check_dfs_target(&self, named: &str, target_addr: &str, asked: &str) -> Result<()> {
        if self.connections.is_primary(target_addr) {
            return Ok(());
        }
        self.config
            .dfs_target_policy
            .check(self.connections.primary_server(), named, target_addr)
            .map_err(|sign_in| dfs_target_not_allowed(asked, sign_in))
    }

    /// Ensure a connection exists in the pool for the given server address,
    /// which a referral named `named`.
    ///
    /// ❌ **The one place this client signs in to another server, and it asks
    /// [`DfsTargetPolicy`] first.** Nothing else may dial a target: a referral
    /// comes from the server, so it can name a machine that collects the
    /// credentials sent here.
    async fn ensure_connection(
        &mut self,
        named: &str,
        target_addr: &str,
        asked: &str,
    ) -> Result<()> {
        self.check_dfs_target(named, target_addr, asked)?;
        if self.connections.is_primary(target_addr) {
            return Ok(()); // Already have primary connection.
        }
        self.connections.forget_dead_target(target_addr);
        if self.connections.extra(target_addr).is_some() {
            return Ok(()); // Already in pool.
        }

        // Create new connection to target.
        let mut conn = Connection::connect_with(target_addr, self.config.connect_options()).await?;
        conn.set_compression_requested(self.config.compression);
        // The primary's deadline as it stands now, not the config's: the
        // consumer may have tuned it since `connect`. Before NEGOTIATE, so a
        // target that accepts and never answers can't hang the call.
        conn.set_response_timeout(self.connections.primary().response_timeout());
        conn.negotiate().await?;

        // Authenticate with same credentials.
        let session = Session::setup(
            &mut conn,
            &self.config.username,
            &self.config.password,
            &self.config.domain,
        )
        .await?;

        // Arm it the same way the primary is. On a namespace root the target
        // connection carries *all* of the user's traffic, so leaving it
        // unrevivable meant `auto_reconnect` silently did not cover the only
        // connection that mattered. The reviver is built for this address, not
        // `config.addr`, which would dial the namespace server back.
        if self.config.auto_reconnect {
            conn.set_reviver(Some(std::sync::Arc::new(ClientReviver::for_target(
                &self.config,
                named,
                target_addr.to_string(),
            ))));
        }
        // And it inherits the bounds the consumer set on the primary, rather
        // than silently falling back to the defaults.
        conn.set_reconnect_policy(self.connections.primary().reconnect_policy());

        self.connections.insert_extra(
            target_addr.to_string(),
            ConnectionEntry {
                conn,
                session: std::sync::Arc::new(session),
            },
        );
        Ok(())
    }

    /// Ensure a tree-connect exists for the given server and share.
    async fn ensure_tree(&mut self, target_addr: &str, share: &str) -> Result<Tree> {
        // `tree.server` is the full addr:port, so `Connections::for_tree` can
        // tell apart targets sharing a hostname on different ports (Docker
        // port-mapped containers, for one).
        //
        // Two branches rather than one, because the primary connection's
        // session lives beside it on `self` while an extra connection carries
        // its own, authenticated separately.
        if self.connections.is_primary(target_addr) {
            self.refresh_session();
            let mut tree = Tree::connect(self.connections.primary_mut(), share).await?;
            tree.server = target_addr.to_string();
            activate_share_encryption(self.connections.primary_mut(), &self.session, &tree);
            return Ok(tree);
        }

        let entry = self
            .connections
            .extra_mut(target_addr)
            .ok_or_else(|| Error::invalid_data("DFS: no connection for target"))?;
        let mut tree = Tree::connect(&mut entry.conn, share).await?;
        tree.server = target_addr.to_string();
        activate_share_encryption(&mut entry.conn, &entry.session, &tree);
        Ok(tree)
    }

    /// Whether this failure means "the session is gone" and auto-reconnect is
    /// armed to do something about it.
    ///
    /// Deliberately narrow. [`Error::CreditStarvation`], [`Error::SendTimeout`]
    /// and a plain [`Error::Timeout`] also smell like a dead link, but none of
    /// them proves the session died, and re-running an operation against a
    /// connection that is merely struggling buys a duplicate request rather
    /// than a recovery. These two are the ones that mean it: `Disconnected`
    /// says the socket went away, and `ServerUnresponsive` is only reached
    /// when a request burned its whole deadline on a connection that put
    /// nothing at all on the wire — and it leaves the connection marked dead,
    /// which is what gives `reconnect_if_needed` something to revive.
    /// Asked of the connection the tree actually lives on, which for a DFS
    /// target is not the primary one.
    fn session_is_gone(&self, tree: &Tree, err: &Error) -> bool {
        self.config.auto_reconnect
            && matches!(
                err,
                Error::Disconnected
                    | Error::ServerUnresponsive { .. }
                    | Error::HandleInvalidated { .. }
            )
            && self
                .connections
                .for_tree_ref(tree)
                .is_ok_and(|conn| conn.can_reconnect())
    }

    /// Bring the tree's own connection back and re-establish `tree` on the new
    /// session.
    ///
    /// Works for a DFS target as well as the primary. A namespace root makes
    /// the *target* connection the one carrying all of the user's traffic, so
    /// "only the primary recovers" meant the connection that mattered did not.
    /// Reviving one says nothing about the other: each has its own socket and
    /// its own session, and only the tree's own is touched.
    async fn recover_tree(&mut self, tree: &mut Tree) -> Result<()> {
        let share = tree.share_name.clone();

        if self.connections.is_primary(&tree.server) {
            self.connections.primary_mut().reconnect_if_needed().await?;
            self.refresh_session();
            // In place, exactly as the DFS redirect does: the caller keeps
            // using the `&mut Tree` it passed in, now pointing at the new
            // session's tree id.
            *tree = self.connect_share(&share).await?;
            return Ok(());
        }

        let addr = tree.server.clone();
        let entry = self
            .connections
            .extra_mut(&addr)
            .ok_or(Error::Disconnected)?;
        entry.conn.reconnect_if_needed().await?;
        // Adopt whatever session the revival established. ❌ Skipping this is
        // not cosmetic: `ensure_tree` activates share encryption from these
        // keys, and the dead session's keys decrypt nothing.
        if let Some(current) = entry.conn.current_session() {
            entry.session = current;
        }
        *tree = self.ensure_tree(&addr, &share).await?;
        Ok(())
    }

    // ── Convenience methods that delegate to Tree ──────────────────────
    //
    // Every path-taking method here follows a DFS link inside the share once,
    // through `follow_dfs_link`, and leaves the caller's tree on its share. The
    // ones taking `&mut Tree` replay after a reconnect, which renews its tree
    // id. See `client/CLAUDE.md` § DFS.

    /// List files in a directory on the given share.
    ///
    /// This is a convenience wrapper around [`Tree::list_directory`] that
    /// saves you from threading `connection_mut()` through every call.
    /// A path behind a DFS link is listed on the link's target; `tree` stays on
    /// its own share either way.
    pub async fn list_directory(
        &mut self,
        tree: &mut Tree,
        path: &str,
    ) -> Result<Vec<DirectoryEntry>> {
        self.list_directory_with(tree, path, ListOptions::new())
            .await
    }

    /// [`list_directory`](Self::list_directory), asking the server for what
    /// `options` names as well (see [`ListOptions`]).
    ///
    /// ```no_run
    /// # async fn example(client: &mut smb2::SmbClient, share: &mut smb2::Tree) -> Result<(), smb2::Error> {
    /// let options = smb2::ListOptions::new().index_numbers(true);
    /// for entry in client.list_directory_with(share, "photos", options).await? {
    ///     println!("{} {:?}", entry.name, entry.index_number);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn list_directory_with(
        &mut self,
        tree: &mut Tree,
        path: &str,
        options: ListOptions,
    ) -> Result<Vec<DirectoryEntry>> {
        self.replaying(tree, path, |mut conn, tree, path| async move {
            tree.list_directory_with(&mut conn, &path, options).await
        })
        .await
        .map(|(value, _)| value)
    }

    /// Open a directory on the given share for listing one server batch at a
    /// time. See [`DirectoryReader`] and [`Tree::open_directory_reader`].
    ///
    /// A path behind a DFS link is listed on the link's target; `tree` stays on
    /// its own share either way. A session found dead at the open is revived
    /// and the open sent again, like [`list_directory`](Self::list_directory),
    /// since nothing has been listed yet. A listing in progress is never
    /// replayed: after a reconnect the reader fails with
    /// [`Error::HandleInvalidated`], and the caller opens a new one.
    pub async fn open_directory_reader(
        &mut self,
        tree: &mut Tree,
        path: &str,
    ) -> Result<DirectoryReader> {
        self.open_directory_reader_with(tree, path, ListOptions::new())
            .await
    }

    /// [`open_directory_reader`](Self::open_directory_reader), asking the
    /// server for what `options` names as well (see [`ListOptions`]).
    pub async fn open_directory_reader_with(
        &mut self,
        tree: &mut Tree,
        path: &str,
        options: ListOptions,
    ) -> Result<DirectoryReader> {
        self.replaying(tree, path, |conn, tree, path| async move {
            tree.open_directory_reader_with(conn, &path, options).await
        })
        .await
        .map(|(reader, _)| reader)
    }

    /// Read a file from the given share.
    pub async fn read_file(&mut self, tree: &mut Tree, path: &str) -> Result<Vec<u8>> {
        self.replaying(tree, path, |mut conn, tree, path| async move {
            tree.read_file(&mut conn, &path).await
        })
        .await
        .map(|(value, _)| value)
    }

    /// Read a small file using a compound CREATE+READ+CLOSE request.
    ///
    /// Sends all three operations in a single transport frame, reducing
    /// round-trips from 3 to 1. Best for files that fit in a single
    /// READ (up to MaxReadSize, typically 8 MB).
    pub async fn read_file_compound(&mut self, tree: &mut Tree, path: &str) -> Result<Vec<u8>> {
        self.replaying(tree, path, |mut conn, tree, path| async move {
            tree.read_file_compound(&mut conn, &path).await
        })
        .await
        .map(|(value, _)| value)
    }

    /// [`read_file_compound`](Self::read_file_compound), plus the file's
    /// metadata (size, times) from the same CREATE response, at no extra
    /// cost. See [`Tree::read_file_compound_with_info`].
    pub async fn read_file_compound_with_info(
        &mut self,
        tree: &mut Tree,
        path: &str,
    ) -> Result<(Vec<u8>, FileInfo)> {
        self.replaying(tree, path, |mut conn, tree, path| async move {
            tree.read_file_compound_with_info(&mut conn, &path).await
        })
        .await
        .map(|(value, _)| value)
    }

    /// Read a file using pipelined I/O (faster for large files).
    pub async fn read_file_pipelined(&mut self, tree: &mut Tree, path: &str) -> Result<Vec<u8>> {
        self.replaying(tree, path, |mut conn, tree, path| async move {
            tree.read_file_pipelined(&mut conn, &path).await
        })
        .await
        .map(|(value, _)| value)
    }

    /// Write data to a file on the given share (create or overwrite).
    pub async fn write_file(&mut self, tree: &Tree, path: &str, data: &[u8]) -> Result<u64> {
        self.beside_tree(tree, path, |mut conn, tree, path| async move {
            tree.write_file(&mut conn, &path, data).await
        })
        .await
        .map(|(done, _)| done)
    }

    /// Write a small file: a compound CREATE+WRITE+FLUSH, then a standalone
    /// CLOSE.
    ///
    /// Two round trips instead of four. Best for files that fit in
    /// MaxWriteSize (typically 64 KB to 8 MB). For larger files, use
    /// [`write_file_pipelined`](Self::write_file_pipelined). Why the CLOSE
    /// can't ride along: [`Tree::write_file_compound`].
    pub async fn write_file_compound(
        &mut self,
        tree: &Tree,
        path: &str,
        data: &[u8],
    ) -> Result<u64> {
        self.beside_tree(tree, path, |mut conn, tree, path| async move {
            tree.write_file_compound(&mut conn, &path, data).await
        })
        .await
        .map(|(done, _)| done)
    }

    /// Write a small NEW file: a compound CREATE+WRITE+FLUSH, then a standalone
    /// CLOSE.
    ///
    /// Like [`write_file_compound`](Self::write_file_compound), but the name
    /// must be free: if it exists, the write is refused with
    /// [`crate::ErrorKind::AlreadyExists`] and the existing file is left
    /// untouched. See [`Tree::write_file_compound_exclusive`].
    pub async fn write_file_compound_exclusive(
        &mut self,
        tree: &Tree,
        path: &str,
        data: &[u8],
    ) -> Result<u64> {
        self.beside_tree(tree, path, |mut conn, tree, path| async move {
            tree.write_file_compound_exclusive(&mut conn, &path, data)
                .await
        })
        .await
        .map(|(done, _)| done)
    }

    /// Write data to a file using pipelined I/O (faster for large files).
    pub async fn write_file_pipelined(
        &mut self,
        tree: &Tree,
        path: &str,
        data: &[u8],
    ) -> Result<u64> {
        self.beside_tree(tree, path, |mut conn, tree, path| async move {
            tree.write_file_pipelined(&mut conn, &path, data).await
        })
        .await
        .map(|(done, _)| done)
    }

    /// Query file system space information for the given share.
    ///
    /// Returns total capacity, free space, and allocation unit sizes.
    /// Uses a compound CREATE+QUERY_INFO+CLOSE for efficiency (one round-trip).
    pub async fn fs_info(&mut self, tree: &mut Tree) -> Result<tree::FsInfo> {
        // No path: a referral here is for the share's root.
        self.replaying(tree, "", |mut conn, tree, _| async move {
            tree.fs_info(&mut conn).await
        })
        .await
        .map(|(value, _)| value)
    }

    /// Delete a file on the given share.
    pub async fn delete_file(&mut self, tree: &Tree, path: &str) -> Result<()> {
        self.beside_tree(tree, path, |mut conn, tree, path| async move {
            tree.delete_file(&mut conn, &path).await
        })
        .await
        .map(|(done, _)| done)
    }

    /// Delete a junction, symlink, or other link on the given share, leaving
    /// what it points to alone. See [`Tree::delete_link`].
    pub async fn delete_link(&mut self, tree: &Tree, path: &str) -> Result<()> {
        self.beside_tree(tree, path, |mut conn, tree, path| async move {
            tree.delete_link(&mut conn, &path).await
        })
        .await
        .map(|(done, _)| done)
    }

    /// Delete multiple files on the given share.
    ///
    /// Returns results in the same order as the input paths.
    ///
    /// Each item costs one round trip and they do not overlap, so this saves
    /// the per-call setup rather than the wire time. To overlap server work,
    /// run the single-item call on several connections concurrently.
    ///
    /// Items behind a DFS link follow it, asking for the referral once per
    /// batch; `tree` stays on its own share, where the other items are.
    pub async fn delete_files(&mut self, tree: &Tree, paths: &[&str]) -> Vec<Result<()>> {
        let mut results = Vec::with_capacity(paths.len());
        for path in paths {
            let result = self
                .beside_tree(tree, path, |mut conn, tree, path| async move {
                    tree.delete_file(&mut conn, &path).await
                })
                .await;
            results.push(result.map(|(done, _)| done));
        }
        results
    }

    /// Get file metadata (size, timestamps, whether it's a directory).
    pub async fn stat(&mut self, tree: &mut Tree, path: &str) -> Result<FileInfo> {
        self.replaying(tree, path, |mut conn, tree, path| async move {
            tree.stat(&mut conn, &path).await
        })
        .await
        .map(|(value, _)| value)
    }

    /// [`stat`](Self::stat), plus the file's [`FileIdentity`] in the same
    /// round trip, or `None` when the server won't say in full. See
    /// [`Tree::stat_with_identity`].
    ///
    /// Follows a DFS link and replays after a reconnect, like `stat`.
    pub async fn stat_with_identity(
        &mut self,
        tree: &mut Tree,
        path: &str,
    ) -> Result<(FileInfo, Option<FileIdentity>)> {
        self.replaying(tree, path, |mut conn, tree, path| async move {
            tree.stat_with_identity(&mut conn, &path).await
        })
        .await
        .map(|(value, _)| value)
    }

    /// Ask the server which file `path` names, and what it calls it: the
    /// stored path (case and 8.3 aliases resolved), the metadata `stat`
    /// returns, and the file's identity. See [`Tree::resolve`].
    ///
    /// Follows a DFS link and replays after a reconnect, like
    /// [`stat`](Self::stat). Behind a link, the returned path is still relative
    /// to `tree`'s share, link folder included, so it opens the same file.
    #[doc(alias = "canonicalize")]
    #[doc(alias = "realpath")]
    pub async fn resolve(&mut self, tree: &mut Tree, path: &str) -> Result<resolve::Resolved> {
        let (mut resolved, link) = self
            .replaying(tree, path, |mut conn, tree, path| async move {
                tree.resolve(&mut conn, &path).await
            })
            .await?;
        if let Some(link) = link {
            resolved.path = link.caller_path(&resolved.path);
        }
        Ok(resolved)
    }

    /// Stat multiple files on the given share.
    ///
    /// Returns results in the same order as the input paths. To describe a
    /// whole directory, `list_directory` is far cheaper than a stat per entry.
    ///
    /// Each item costs one round trip and they do not overlap, so this saves
    /// the per-call setup rather than the wire time. To overlap server work,
    /// run the single-item call on several connections concurrently.
    ///
    /// Items behind a DFS link follow it, asking for the referral once per
    /// batch; `tree` stays on its own share, where the other items are.
    pub async fn stat_files(&mut self, tree: &Tree, paths: &[&str]) -> Vec<Result<FileInfo>> {
        let mut results = Vec::with_capacity(paths.len());
        for path in paths {
            let result = self
                .beside_tree(tree, path, |mut conn, tree, path| async move {
                    tree.stat(&mut conn, &path).await
                })
                .await;
            results.push(result.map(|(info, _)| info));
        }
        results
    }

    /// Rename a file or directory on the given share.
    ///
    /// Behind a DFS link, a rename follows the link when `to` sits inside the
    /// same link folder, across subfolders too (`data/a/x` to `data/b/x`). A
    /// `to` outside it would be a move to another share, which no single SMB
    /// rename can do, so it keeps the server's `STATUS_PATH_NOT_COVERED`.
    pub async fn rename(&mut self, tree: &Tree, from: &str, to: &str) -> Result<()> {
        self.rename_with(tree, from, to, RenameOptions::default())
            .await
    }

    /// Rename like [`rename`](Self::rename), with `options` deciding what
    /// happens to a file already at `to`. See [`Tree::rename_with`].
    ///
    /// # Example
    ///
    /// Save over a file without a moment where it's missing: write the new
    /// contents beside it, then swap them in.
    ///
    /// ```no_run
    /// # async fn example(client: &mut smb2::SmbClient, share: &smb2::Tree) -> Result<(), smb2::Error> {
    /// use smb2::RenameOptions;
    ///
    /// client.write_file(share, "notes.txt.tmp", b"new contents").await?;
    /// client
    ///     .rename_with(share, "notes.txt.tmp", "notes.txt", RenameOptions::new().replace_if_exists(true))
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn rename_with(
        &mut self,
        tree: &Tree,
        from: &str,
        to: &str,
        options: RenameOptions,
    ) -> Result<()> {
        // Two steps behind a link: following it says where `from` lives, and
        // the rename's destination, which SET_INFO takes share-relative with
        // no DFS prefix, needs the same link to move along with it. So the
        // op renames only on the caller's own tree, and on a link's target
        // hands the path back for the rename below.
        let caller = (tree.tree_id, tree.server.clone());
        let mut op = |mut conn: Connection, on: Tree, path: String| {
            let on_caller_tree = (on.tree_id, &on.server) == (caller.0, &caller.1);
            async move {
                if !on_caller_tree {
                    return Ok(Some(path));
                }
                on.rename_with(&mut conn, &path, to, options)
                    .await
                    .map(|()| None)
            }
        };
        let followed = self.follow_dfs_link(tree, from, &mut op).await;
        let outcome = match followed.result {
            Ok(outcome) => outcome,
            Err(e) => return Err(self.link_loss(tree, from, followed.moved_to.as_ref(), e)),
        };
        let (remaining, link) = match (outcome, followed.moved_to) {
            (None, _) => return Ok(()),
            (Some(remaining), Some(link)) => (remaining, link),
            (Some(_), None) => return Err(Error::invalid_data("DFS: a rename lost its link")),
        };
        // A destination outside the link is a move between shares, which no
        // single SMB rename can do: the server's own answer stands.
        let Some(target_to) = link.target_path(to) else {
            return Err(Error::Protocol {
                status: NtStatus::PATH_NOT_COVERED,
                command: crate::types::Command::Create,
            });
        };
        let mut conn = self.connections.for_tree(&link.tree)?.clone();
        let renamed = link
            .tree
            .rename_with(&mut conn, &remaining, &target_to, options)
            .await;
        renamed.map_err(|e| self.link_loss(tree, from, Some(&link), e))
    }

    /// Rename multiple files on the given share.
    ///
    /// Returns results in the same order as the input pairs.
    ///
    /// Each item costs one round trip and they do not overlap, so this saves
    /// the per-call setup rather than the wire time. To overlap server work,
    /// run the single-item call on several connections concurrently.
    ///
    /// Items behind a DFS link follow it like [`rename`](Self::rename), asking
    /// for the referral once per batch; `tree` stays on its own share, where
    /// the other items are.
    pub async fn rename_files(&mut self, tree: &Tree, renames: &[(&str, &str)]) -> Vec<Result<()>> {
        self.rename_files_with(tree, renames, RenameOptions::default())
            .await
    }

    /// Rename multiple files like [`rename_files`](Self::rename_files), each
    /// with the same `options` (see [`rename_with`](Self::rename_with)).
    pub async fn rename_files_with(
        &mut self,
        tree: &Tree,
        renames: &[(&str, &str)],
        options: RenameOptions,
    ) -> Vec<Result<()>> {
        let mut results = Vec::with_capacity(renames.len());
        for (from, to) in renames {
            results.push(self.rename_with(tree, from, to, options).await);
        }
        results
    }

    /// Set a file's or directory's timestamps (and attributes, when `times`
    /// carries them), leaving whatever `times` doesn't name as it is. See
    /// [`Tree::set_times`], including why a
    /// writer's own times are best set with
    /// [`FileWriter::set_times`](crate::FileWriter::set_times).
    ///
    /// Follows a DFS link; `tree` stays on its own share.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(client: &mut smb2::SmbClient, share: &smb2::Tree) -> Result<(), smb2::Error> {
    /// use std::time::{Duration, SystemTime};
    /// use smb2::FileTimes;
    ///
    /// let taken = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000);
    /// client.set_times(share, "photos/beach.jpg", FileTimes::new().set_modified(taken)).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn set_times(&mut self, tree: &Tree, path: &str, times: FileTimes) -> Result<()> {
        self.beside_tree(tree, path, |mut conn, tree, path| async move {
            tree.set_times(&mut conn, &path, times).await
        })
        .await
        .map(|(done, _)| done)
    }

    /// Create a directory on the given share.
    pub async fn create_directory(&mut self, tree: &Tree, path: &str) -> Result<()> {
        self.beside_tree(tree, path, |mut conn, tree, path| async move {
            tree.create_directory(&mut conn, &path).await
        })
        .await
        .map(|(done, _)| done)
    }

    /// Delete an empty directory on the given share.
    pub async fn delete_directory(&mut self, tree: &Tree, path: &str) -> Result<()> {
        self.beside_tree(tree, path, |mut conn, tree, path| async move {
            tree.delete_directory(&mut conn, &path).await
        })
        .await
        .map(|(done, _)| done)
    }

    /// Start a streaming file download (memory-efficient for large files).
    ///
    /// Returns a [`FileDownload`] that yields chunks one at a time without
    /// buffering the entire file in memory, with the same adaptive read-ahead
    /// window as [`Tree::download`].
    ///
    /// The client stays borrowed for the download's life. To hold it for the
    /// open alone (to run other calls meanwhile, or to spawn the download onto
    /// its own task), use [`open_file_download`](Self::open_file_download),
    /// which this delegates to.
    ///
    /// Follows a DFS link: the download reads from the link's target, and
    /// `tree` stays on its own share.
    ///
    /// # Example
    ///
    /// ```ignore
    /// # async fn example(client: &mut smb2::SmbClient, share: &smb2::Tree) -> Result<(), smb2::Error> {
    /// use tokio::io::AsyncWriteExt;
    ///
    /// let mut download = client.download(&share, "big_video.mp4").await?;
    /// println!("Downloading {} bytes...", download.size());
    ///
    /// let mut file = tokio::fs::File::create("big_video.mp4").await?;
    /// while let Some(chunk) = download.next_chunk().await {
    ///     let bytes = chunk?;
    ///     file.write_all(&bytes).await?;
    ///     println!("{:.1}%", download.progress().percent());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn download<'a>(
        &'a mut self,
        tree: &'a Tree,
        path: &str,
    ) -> Result<FileDownload<'a>> {
        self.open_file_download(tree, path).await
    }

    /// Start a streaming file download that doesn't borrow the client.
    ///
    /// The same download as [`download`](Self::download) (chunked, with the
    /// adaptive read-ahead window of [`Tree::download`]), but the returned
    /// [`FileDownload`] owns a clone of the connection and of the tree it
    /// reads from (both cheap), so it's `'static` and the client is held for
    /// the CREATE only, like [`open_file_reader`](Self::open_file_reader).
    /// Several downloads opened this way stream over one SMB session in
    /// parallel.
    ///
    /// Follows a DFS link: the download reads from the link's target, and
    /// `tree` stays on its own share. It's the call for a consumer that would
    /// otherwise read a link's target through a
    /// [`FileReader`](stream::FileReader) (positioned reads, no read-ahead)
    /// only to avoid holding the client.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(client: &mut smb2::SmbClient, share: &mut smb2::Tree) -> Result<(), smb2::Error> {
    /// let mut download = client.open_file_download(share, "big_video.mp4").await?;
    /// let task = tokio::spawn(async move {
    ///     let mut total = 0;
    ///     while let Some(chunk) = download.next_chunk().await {
    ///         total += chunk?.len();
    ///     }
    ///     Ok::<_, smb2::Error>(total)
    /// });
    /// // The client is free for other calls while the download runs.
    /// let entries = client.list_directory(share, "").await?;
    /// # let _ = (task, entries);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn open_file_download(
        &mut self,
        tree: &Tree,
        path: &str,
    ) -> Result<FileDownload<'static>> {
        // Pinned before the open, and handed out with the handle: the
        // download sends only on the session its handle belongs to. The tree
        // comes back owned: the caller's, or the link's target when the call
        // was redirected (the caller's own tree never moves).
        let ((file_id, info, conn, tree), _) = self
            .beside_tree(tree, path, |conn, tree, path| async move {
                let mut conn = tree.pinned_conn(&conn).pinned_to_session();
                let (file_id, info) = tree.open_for_read(&mut conn, &path).await?;
                Ok((file_id, info, conn, tree))
            })
            .await?;
        Ok(FileDownload::of_open_file(
            Cow::Owned(tree),
            conn,
            file_id,
            info,
        ))
    }

    /// Start a streaming file upload with progress tracking.
    ///
    /// Returns a [`FileUpload`] that writes data in chunks. Each call to
    /// [`write_next_chunk`](FileUpload::write_next_chunk) sends one WRITE
    /// request and reports progress.
    ///
    /// For small files (data fits in one MaxWriteSize), the data is written
    /// immediately in the constructor, via a compound CREATE+WRITE+FLUSH and
    /// a standalone CLOSE. The returned `FileUpload` is already complete, and
    /// `write_next_chunk` returns `false` immediately. This gives the caller
    /// a uniform API regardless of file size.
    ///
    /// The connection is borrowed mutably for the lifetime of the upload,
    /// so no other operations can run concurrently. This prevents accidental
    /// interleaving of SMB messages.
    ///
    /// Follows a DFS link: the upload writes to the link's target, and `tree`
    /// stays on its own share.
    ///
    /// # Example
    ///
    /// ```ignore
    /// # async fn example(client: &mut smb2::SmbClient, share: &smb2::Tree) -> Result<(), smb2::Error> {
    /// let data = std::fs::read("large_video.mp4")?;
    /// let mut upload = client.upload(&share, "remote_video.mp4", &data).await?;
    /// println!("Uploading {} bytes...", upload.total_bytes());
    ///
    /// while upload.write_next_chunk().await? {
    ///     println!("{:.1}%", upload.progress().percent());
    /// }
    /// // File is flushed and closed automatically after the last chunk.
    /// # Ok(())
    /// # }
    /// ```
    pub async fn upload<'a>(
        &'a mut self,
        tree: &'a Tree,
        path: &str,
        data: &'a [u8],
    ) -> Result<stream::FileUpload<'a>> {
        // `Some(handle)` for a file too big for one frame, `None` for one the
        // compound already wrote. Decided on the connection the write lands
        // on, which behind a link is the target's.
        // Pinned before the open, and handed out with the handle: the upload
        // sends only on the session its handle belongs to.
        let ((opened, conn), moved_to) = self
            .beside_tree(tree, path, |conn, tree, path| async move {
                let mut conn = tree.pinned_conn(&conn).pinned_to_session();
                if data.len() as u64 <= conn.compound_write_limit() {
                    // The limit is `max_write`, lowered to what the credit
                    // window funds. A refused compound CREATE fails the WRITE
                    // with it, so a retry behind a link writes nothing twice.
                    tree.write_file_compound(&mut conn, &path, data).await?;
                    Ok((None, conn))
                } else {
                    let file_id = tree.open_file_for_write(&mut conn, &path).await?;
                    Ok((Some(file_id), conn))
                }
            })
            .await?;
        let tree = moved_to.map_or(Cow::Borrowed(tree), |link| Cow::Owned(link.tree));
        Ok(match opened {
            None => stream::FileUpload::new_done(tree, conn, data.len() as u64),
            Some(file_id) => {
                let max_write = conn.params().map_or(65536, |p| p.max_write_size);
                stream::FileUpload::new(tree, conn, file_id, data, max_write)
            }
        })
    }

    /// Open a random-access [`FileReader`](stream::FileReader) over a file on
    /// the share.
    ///
    /// Clones the tree's connection (cheap `Arc::clone`) and the
    /// `Tree`, then returns a reader that serves any number of positioned reads
    /// at arbitrary offsets over one open handle. The returned reader is
    /// `'static` and does not borrow the client, so concurrent readers proceed
    /// in parallel over the single SMB session. Call
    /// [`FileReader::close`](stream::FileReader::close) when done.
    ///
    /// Follows a DFS link: the reader reads from the link's target, and `tree`
    /// stays on its own share.
    pub async fn open_file_reader(
        &mut self,
        tree: &Tree,
        path: &str,
    ) -> Result<stream::FileReader> {
        self.beside_tree(tree, path, |conn, tree, path| async move {
            stream::open_file_reader(std::sync::Arc::new(tree), conn, &path).await
        })
        .await
        .map(|(reader, _)| reader)
    }

    /// Create a push-based pipelined streaming file writer.
    ///
    /// Opens (or creates) the file for writing and returns a [`FileWriter`]
    /// that the caller drives by pushing data chunks. The returned writer
    /// owns a cheap `Arc::clone` of `Connection` and an `Arc<Tree>` — it
    /// is `'static` and does not borrow from the client. Multiple writers
    /// built this way pipeline their WRITEs over a single SMB session
    /// without external locking.
    ///
    /// Follows a DFS link: the writer writes to the link's target, and `tree`
    /// stays on its own share.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(client: &mut smb2::SmbClient, share: &smb2::Tree) -> Result<(), smb2::Error> {
    /// let mut writer = client.create_file_writer(share, "output.bin").await?;
    /// writer.write_chunk(b"hello").await?;
    /// writer.write_chunk(b" world").await?;
    /// let total = writer.finish().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn create_file_writer(
        &mut self,
        tree: &Tree,
        path: &str,
    ) -> Result<stream::FileWriter> {
        // The writer owns a clone of the target's connection (cheap
        // `Arc::clone`) and of its `Tree`, so the client isn't borrowed for
        // the upload's duration and concurrent writers proceed in parallel.
        self.beside_tree(tree, path, |conn, tree, path| async move {
            stream::open_file_writer(std::sync::Arc::new(tree), conn, &path).await
        })
        .await
        .map(|(writer, _)| writer)
    }

    /// Exclusive-create sibling of [`create_file_writer`](Self::create_file_writer).
    ///
    /// Same shape, but the CREATE uses `FileCreate` disposition: if the file
    /// already exists the open fails with
    /// [`crate::ErrorKind::AlreadyExists`]. Use
    /// this for race-free "create only if absent" writes — for example, a
    /// file manager's "New File" action where silently clobbering an
    /// existing file is unsafe.
    pub async fn create_file_writer_exclusive(
        &mut self,
        tree: &Tree,
        path: &str,
    ) -> Result<stream::FileWriter> {
        self.beside_tree(tree, path, |conn, tree, path| async move {
            stream::open_file_writer_exclusive(std::sync::Arc::new(tree), conn, &path).await
        })
        .await
        .map(|(writer, _)| writer)
    }

    /// Create a positioned push-based streaming file writer.
    ///
    /// Same shape as [`create_file_writer`](Self::create_file_writer), but the
    /// file is opened without truncating and the writer's first byte lands at
    /// `offset`. Use it to append after a server-side-copied prefix (see
    /// [`server_side_copy_range`](Tree::server_side_copy_range)) or to patch a
    /// known region of an existing file.
    ///
    /// To carry on an interrupted upload, use
    /// [`resume_file_writer`](Self::resume_file_writer) instead: this one
    /// trusts that the file is still the one you were writing.
    pub async fn create_file_writer_at(
        &mut self,
        tree: &Tree,
        path: &str,
        offset: u64,
    ) -> Result<stream::FileWriter> {
        self.beside_tree(tree, path, |conn, tree, path| async move {
            stream::open_file_writer_at(std::sync::Arc::new(tree), conn, &path, offset).await
        })
        .await
        .map(|(writer, _)| writer)
    }

    /// Carry on an interrupted upload from a checkpoint
    /// ([`FileWriter::flush_checkpoint`](stream::FileWriter::flush_checkpoint)),
    /// once the server has proven `path` is still the file it names and still
    /// holds everything it vouches for.
    ///
    /// One round trip, and nothing is written unless every check holds. A
    /// file that's gone, replaced, shorter than the checkpoint, or that the
    /// server won't identify is refused with
    /// [`Error::ResumeRefused`]: write it again
    /// from the start. Details and an example:
    /// [`open_file_writer_resume`](stream::open_file_writer_resume).
    ///
    /// To patch a region of a file you know about, without any of these
    /// checks, use [`create_file_writer_at`](Self::create_file_writer_at).
    ///
    /// Follows a DFS link like [`create_file_writer`](Self::create_file_writer).
    pub async fn resume_file_writer(
        &mut self,
        tree: &Tree,
        path: &str,
        checkpoint: &stream::WriteCheckpoint,
    ) -> Result<stream::FileWriter> {
        let checkpoint = *checkpoint;
        self.beside_tree(tree, path, move |conn, tree, path| async move {
            stream::open_file_writer_resume(std::sync::Arc::new(tree), conn, &path, &checkpoint)
                .await
        })
        .await
        .map(|(writer, _)| writer)
    }

    /// Read a file with progress reporting and cancellation.
    ///
    /// Uses pipelined I/O for performance, calling `on_progress` after each
    /// chunk is received. Return `ControlFlow::Break(())` to cancel the read.
    ///
    /// Follows a DFS link like [`read_file`](Self::read_file): the file is
    /// opened before the first callback, so a redirect never reports
    /// progress twice.
    pub async fn read_file_with_progress<F>(
        &mut self,
        tree: &Tree,
        path: &str,
        on_progress: F,
    ) -> Result<Vec<u8>>
    where
        F: FnMut(Progress) -> ControlFlow<()>,
    {
        // Only the open goes through the link helper, so no progress is
        // reported before the file is open wherever it lives.
        // Pinned before the open, so the reads after it can't reach a newer
        // session.
        let ((file_id, file_size, mut conn), moved_to) = self
            .beside_tree(tree, path, |conn, tree, path| async move {
                let mut conn = tree.pinned_conn(&conn).pinned_to_session();
                let (file_id, file_size) = tree.open_file(&mut conn, &path).await?;
                Ok((file_id, file_size, conn))
            })
            .await?;
        let tree = moved_to.as_ref().map_or(tree, |link| &link.tree);
        tree.read_open_file_with_progress(&mut conn, path, file_id, file_size, on_progress)
            .await
    }

    /// Write a file with progress reporting and cancellation.
    ///
    /// Writes data in chunks, calling `on_progress` after each chunk.
    /// Return `ControlFlow::Break(())` to cancel the write.
    ///
    /// The file is flushed before closing to ensure data is persisted
    /// on the server.
    ///
    /// Follows a DFS link like [`write_file`](Self::write_file): the file is
    /// opened before the first callback, so a redirect never reports
    /// progress twice.
    pub async fn write_file_with_progress<F>(
        &mut self,
        tree: &Tree,
        path: &str,
        data: &[u8],
        mut on_progress: F,
    ) -> Result<u64>
    where
        F: FnMut(Progress) -> ControlFlow<()>,
    {
        // Open the file for writing, pinned first, so the writes after it
        // can't reach a newer session.
        let ((file_id, mut conn), moved_to) = self
            .beside_tree(tree, path, |conn, tree, path| async move {
                let conn = tree.pinned_conn(&conn).pinned_to_session();
                let req = crate::msg::create::CreateRequest {
                    requested_oplock_level: crate::types::OplockLevel::None,
                    impersonation_level: crate::msg::create::ImpersonationLevel::Impersonation,
                    desired_access: crate::types::flags::FileAccessMask::new(
                        crate::types::flags::FileAccessMask::FILE_WRITE_DATA
                            | crate::types::flags::FileAccessMask::FILE_WRITE_ATTRIBUTES
                            | crate::types::flags::FileAccessMask::SYNCHRONIZE,
                    ),
                    file_attributes: crate::types::flags::FileAttributes::NORMAL,
                    share_access: crate::msg::create::ShareAccess(0),
                    create_disposition: crate::msg::create::CreateDisposition::FileOverwriteIf,
                    create_options: 0x0000_0040, // FILE_NON_DIRECTORY_FILE
                    name: tree.format_path(&path),
                    create_contexts: vec![],
                };

                let frame = conn
                    .execute(crate::types::Command::Create, &req, Some(tree.tree_id))
                    .await?;

                if frame.header.status != NtStatus::SUCCESS {
                    return Err(crate::Error::Protocol {
                        status: frame.header.status,
                        command: crate::types::Command::Create,
                    });
                }

                let mut cursor = crate::pack::ReadCursor::new(&frame.body);
                let file_id = crate::msg::create::CreateResponse::unpack(&mut cursor)?.file_id;
                Ok((file_id, conn))
            })
            .await?;
        // The rest runs where the file was opened: the link's target, if any.
        let tree = moved_to.as_ref().map_or(tree, |link| &link.tree);
        let conn = &mut conn;

        let max_write = conn.params().map(|p| p.max_write_size).unwrap_or(65536);
        let mut pipe = write_pipe::WritePipe::new(conn.clone(), tree.tree_id, file_id, max_write);
        let total_bytes = Some(data.len() as u64);

        // Reported whenever the server has confirmed more, which is what
        // `bytes_transferred` counts.
        let mut reported = 0u64;
        let mut report = |pipe: &write_pipe::WritePipe| -> ControlFlow<()> {
            if pipe.confirmed() == reported {
                return ControlFlow::Continue(());
            }
            reported = pipe.confirmed();
            on_progress(Progress {
                bytes_transferred: reported,
                total_bytes,
            })
        };

        let written = async {
            let mut sent = 0;
            while sent < data.len() {
                let len = (data.len() - sent).min(pipe.next_len() as usize);
                pipe.send(data[sent..sent + len].to_vec()).await?;
                sent += len;
                if report(&pipe).is_break() {
                    return Ok(None);
                }
            }
            while pipe.confirm_next().await? {
                if report(&pipe).is_break() {
                    return Ok(None);
                }
            }
            Ok::<_, crate::Error>(Some(pipe.confirmed()))
        }
        .await;

        let total_written = match written {
            Ok(Some(total)) => total,
            Ok(None) => {
                // Cancelled: wait out what's on the wire, then close without
                // a flush (best-effort).
                pipe.abandon().await;
                let _ = tree.close_handle(conn, file_id).await;
                return Err(crate::Error::Cancelled);
            }
            Err(e) => {
                if matches!(
                    e,
                    crate::Error::Protocol {
                        command: crate::types::Command::Write,
                        ..
                    }
                ) {
                    let _ = tree.close_handle(conn, file_id).await;
                }
                return Err(e);
            }
        };

        // Flush to ensure data is persisted.
        tree.flush_handle(conn, file_id).await?;

        // Close the handle.
        tree.close_handle(conn, file_id).await?;

        Ok(total_written)
    }

    /// Write a file from a streaming source using pipelined I/O.
    ///
    /// Pulls data on demand from a callback, so you never need the full
    /// file in memory. See [`Tree::write_file_streamed`] for the full
    /// callback contract, performance characteristics, and usage guide.
    ///
    /// Follows a DFS link like [`write_file`](Self::write_file): the file is
    /// opened before `next_chunk` is first called, so a redirect never loses
    /// a chunk.
    pub async fn write_file_streamed<F>(
        &mut self,
        tree: &Tree,
        path: &str,
        next_chunk: &mut F,
    ) -> Result<u64>
    where
        F: FnMut() -> Option<std::result::Result<Vec<u8>, std::io::Error>>,
    {
        // Only the open goes through the link helper, so no chunk is pulled
        // before the file is open wherever it lives.
        // Pinned before the open, so the writes after it can't reach a newer
        // session.
        let ((file_id, mut conn), moved_to) = self
            .beside_tree(tree, path, |conn, tree, path| async move {
                let mut conn = tree.pinned_conn(&conn).pinned_to_session();
                let file_id = tree.open_file_for_write(&mut conn, &path).await?;
                Ok((file_id, conn))
            })
            .await?;
        // The rest runs where the file was opened: the link's target, if any.
        let tree = moved_to.as_ref().map_or(tree, |link| &link.tree);
        tree.write_open_file_streamed(&mut conn, file_id, next_chunk)
            .await
    }

    /// Flush a file to ensure data is persisted on the server.
    ///
    /// This sends an SMB2 FLUSH request for the given file handle.
    /// Write methods (`write_file`, `write_file_pipelined`,
    /// `write_file_with_progress`) flush automatically before closing.
    /// Use this if you need to flush a handle obtained through the
    /// low-level API.
    pub async fn flush_file(&mut self, tree: &Tree, file_id: FileId) -> Result<()> {
        let conn = self.connections.for_tree(tree)?;
        tree.flush_handle(conn, file_id).await
    }

    /// Watch a directory for changes.
    ///
    /// Opens the directory and returns a [`Watcher`] that yields change
    /// events. The server holds each request until changes occur (long poll).
    ///
    /// Set `recursive` to `true` to watch the entire subtree.
    ///
    /// The returned `Watcher` owns a cloned connection (cheap `Arc::clone`,
    /// all clones multiplex over the same SMB session), so this client
    /// remains usable for other operations while watching.
    ///
    /// Follows a DFS link: the watcher watches the link's target, and `tree`
    /// stays on its own share.
    pub async fn watch(&mut self, tree: &Tree, path: &str, recursive: bool) -> Result<Watcher> {
        self.beside_tree(tree, path, |mut conn, tree, path| async move {
            tree.watch(&mut conn, &path, recursive).await
        })
        .await
        .map(|(watcher, _)| watcher)
    }

    /// [`watch`](Self::watch) with [`WatchOptions`]: recursion, and whether to
    /// hear about xattr and named-stream changes. See [`Tree::watch_with`].
    ///
    /// Follows a DFS link like `watch`.
    pub async fn watch_with(
        &mut self,
        tree: &Tree,
        path: &str,
        options: WatchOptions,
    ) -> Result<Watcher> {
        self.beside_tree(tree, path, |mut conn, tree, path| async move {
            tree.watch_with(&mut conn, &path, options).await
        })
        .await
        .map(|(watcher, _)| watcher)
    }

    /// List a file's or directory's named streams. See [`Tree::list_streams`].
    ///
    /// Follows a DFS link and replays after a reconnect, like `stat`.
    pub async fn list_streams(&mut self, tree: &mut Tree, path: &str) -> Result<Vec<StreamInfo>> {
        self.replaying(tree, path, |mut conn, tree, path| async move {
            tree.list_streams(&mut conn, &path).await
        })
        .await
        .map(|(value, _)| value)
    }

    /// Read a whole named stream. See [`Tree::read_stream`].
    ///
    /// Follows a DFS link and replays after a reconnect, like `stat`.
    pub async fn read_stream(
        &mut self,
        tree: &mut Tree,
        path: &str,
        stream: &str,
    ) -> Result<Vec<u8>> {
        self.replaying(tree, path, |mut conn, tree, path| async move {
            tree.read_stream(&mut conn, &path, stream).await
        })
        .await
        .map(|(value, _)| value)
    }

    /// Replace a named stream's content. See [`Tree::write_stream`].
    ///
    /// Follows a DFS link. Not replayed after a reconnect, like every write.
    pub async fn write_stream(
        &mut self,
        tree: &Tree,
        path: &str,
        stream: &str,
        data: &[u8],
    ) -> Result<()> {
        self.beside_tree(tree, path, |mut conn, tree, path| async move {
            tree.write_stream(&mut conn, &path, stream, data).await
        })
        .await
        .map(|(done, _)| done)
    }

    /// Delete a named stream. See [`Tree::delete_stream`].
    ///
    /// Follows a DFS link. Not replayed after a reconnect, like every delete.
    pub async fn delete_stream(&mut self, tree: &Tree, path: &str, stream: &str) -> Result<()> {
        self.beside_tree(tree, path, |mut conn, tree, path| async move {
            tree.delete_stream(&mut conn, &path, stream).await
        })
        .await
        .map(|(done, _)| done)
    }

    /// What this share's file system says about itself. See
    /// [`Tree::fs_attributes`].
    pub async fn fs_attributes(&mut self, tree: &mut Tree) -> Result<FsAttributes> {
        self.replaying(tree, "", |mut conn, tree, _| async move {
            tree.fs_attributes(&mut conn).await
        })
        .await
        .map(|(value, _)| value)
    }

    /// Read an xattr. `Ok(None)` when the file has no such attribute. See
    /// [`Tree::get_xattr`].
    ///
    /// Follows a DFS link and replays after a reconnect, like `stat`.
    pub async fn get_xattr(
        &mut self,
        tree: &mut Tree,
        path: &str,
        name: &str,
    ) -> Result<Option<Vec<u8>>> {
        self.replaying(tree, path, |mut conn, tree, path| async move {
            tree.get_xattr(&mut conn, &path, name).await
        })
        .await
        .map(|(value, _)| value)
    }

    /// Write an xattr. See [`Tree::set_xattr`].
    ///
    /// Follows a DFS link. Not replayed after a reconnect, like every write.
    pub async fn set_xattr(
        &mut self,
        tree: &Tree,
        path: &str,
        name: &str,
        value: &[u8],
    ) -> Result<()> {
        self.beside_tree(tree, path, |mut conn, tree, path| async move {
            tree.set_xattr(&mut conn, &path, name, value).await
        })
        .await
        .map(|(done, _)| done)
    }

    /// Remove an xattr. See [`Tree::remove_xattr`].
    ///
    /// Follows a DFS link. Not replayed after a reconnect, like every delete.
    pub async fn remove_xattr(&mut self, tree: &Tree, path: &str, name: &str) -> Result<()> {
        self.beside_tree(tree, path, |mut conn, tree, path| async move {
            tree.remove_xattr(&mut conn, &path, name).await
        })
        .await
        .map(|(done, _)| done)
    }

    /// List a file's xattr names. See [`Tree::list_xattrs`].
    ///
    /// Follows a DFS link and replays after a reconnect, like `stat`.
    pub async fn list_xattrs(&mut self, tree: &mut Tree, path: &str) -> Result<Vec<String>> {
        self.replaying(tree, path, |mut conn, tree, path| async move {
            tree.list_xattrs(&mut conn, &path).await
        })
        .await
        .map(|(value, _)| value)
    }

    /// Read one xattr from many files, overlapped on one connection. Results
    /// are in input order, each independent of the others. See
    /// [`Tree::get_xattr_many`].
    ///
    /// Replays after a reconnect, like `stat`. Items behind a DFS link follow
    /// it, one at a time, asking for the referral once per batch. For a whole
    /// folder behind a link, get its target with
    /// [`resolve_dfs_link`](Self::resolve_dfs_link) and call
    /// [`Tree::get_xattr_many`] on it, which overlaps them.
    pub async fn get_xattr_many(
        &mut self,
        tree: &mut Tree,
        paths: &[&str],
        name: &str,
        options: BatchOptions,
    ) -> Vec<Result<Option<Vec<u8>>>> {
        let mut conn = match self.connections.for_tree(tree) {
            Ok(conn) => conn.clone(),
            Err(_) => return self.orphaned_batch(tree, paths.len()),
        };
        let mut results = tree
            .get_xattr_many(&mut conn, paths, name, options.clone())
            .await;

        // A revived session takes the whole remainder at the pipelined pace.
        let gone: Vec<usize> = (0..paths.len())
            .filter(|&i| matches!(&results[i], Err(e) if self.session_is_gone(tree, e)))
            .collect();
        if !gone.is_empty() && self.recover_tree(tree).await.is_ok() {
            if let Ok(conn) = self.connections.for_tree(tree) {
                let mut conn = conn.clone();
                let retry: Vec<&str> = gone.iter().map(|&i| paths[i]).collect();
                let again = tree
                    .get_xattr_many(&mut conn, &retry, name, options.clone())
                    .await;
                for (&index, result) in gone.iter().zip(again) {
                    results[index] = result;
                }
            }
        }

        for (index, path) in paths.iter().enumerate() {
            if matches!(&results[index], Err(e) if self.is_dfs_link(e)) {
                results[index] = self
                    .replaying(tree, path, |mut conn, tree, path| async move {
                        tree.get_xattr(&mut conn, &path, name).await
                    })
                    .await
                    .map(|(value, _)| value);
            }
        }
        results
    }

    /// Write one xattr on many files, overlapped on one connection, each with
    /// its own value. See [`Tree::set_xattr_many`].
    ///
    /// Not replayed after a reconnect, like every write. Items behind a DFS
    /// link follow it, one at a time.
    pub async fn set_xattr_many(
        &mut self,
        tree: &Tree,
        items: &[(&str, &[u8])],
        name: &str,
        options: BatchOptions,
    ) -> Vec<Result<()>> {
        let mut conn = match self.connections.for_tree(tree) {
            Ok(conn) => conn.clone(),
            Err(_) => return self.orphaned_batch(tree, items.len()),
        };
        let mut results = tree.set_xattr_many(&mut conn, items, name, options).await;
        for (index, (path, value)) in items.iter().enumerate() {
            if matches!(&results[index], Err(e) if self.is_dfs_link(e)) {
                results[index] = self
                    .beside_tree(tree, path, |mut conn, tree, path| async move {
                        tree.set_xattr(&mut conn, &path, name, value).await
                    })
                    .await
                    .map(|(done, _)| done);
            }
        }
        results
    }

    /// One "this tree isn't on this client" error per item.
    fn orphaned_batch<T>(&mut self, tree: &Tree, count: usize) -> Vec<Result<T>> {
        (0..count)
            .map(|_| {
                self.connections
                    .for_tree(tree)
                    .map(|_| unreachable!("the caller found this tree orphaned"))
            })
            .collect()
    }

    /// Disconnect from a share.
    pub async fn disconnect_share(&mut self, tree: &Tree) -> Result<()> {
        // A link's pooled target tree is never handed to the caller, but a
        // `Watcher` or `FileReader` carries a clone, so one could come back
        // here; stop reusing it either way.
        self.connections.forget_link_tree(tree);
        let conn = self.connections.for_tree(tree)?;
        tree.disconnect(conn).await
    }
}

/// Connect to an SMB server with the simplest possible API.
///
/// This is a shorthand for creating a [`ClientConfig`] and calling
/// [`SmbClient::connect`]. Uses a five-second timeout and no auto-reconnect.
pub async fn connect(addr: &str, username: &str, password: &str) -> Result<SmbClient> {
    SmbClient::connect(ClientConfig {
        addr: addr.to_string(),
        timeout: Duration::from_secs(5),
        username: username.to_string(),
        password: password.to_string(),
        domain: String::new(),
        auto_reconnect: false,
        compression: true,
        dfs_enabled: true,
        dfs_target_overrides: std::collections::HashMap::new(),
        connect_options: None,
        ..Default::default()
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::connection::pack_message;
    use crate::client::test_helpers::build_tree_connect_response;
    use crate::msg::header::Header;
    use crate::msg::negotiate::{NegotiateContext, NegotiateResponse, HASH_ALGORITHM_SHA512};
    use crate::msg::session_setup::{SessionFlags, SessionSetupResponse};
    use crate::msg::tree_connect::ShareType;
    use crate::pack::Guid;
    use crate::transport::MockTransport;
    use crate::types::flags::{Capabilities, SecurityMode};
    use crate::types::status::NtStatus;
    use crate::types::{Command, Dialect, SessionId, TreeId};
    use std::sync::Arc;

    /// Build a negotiate response.
    fn build_negotiate_response() -> Vec<u8> {
        let mut h = Header::new_request(Command::Negotiate);
        h.flags.set_response();
        h.credits = 32;
        let body = NegotiateResponse {
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            dialect_revision: Dialect::Smb3_1_1,
            server_guid: Guid::ZERO,
            capabilities: Capabilities::new(Capabilities::DFS | Capabilities::LEASING),
            max_transact_size: 65536,
            max_read_size: 65536,
            max_write_size: 65536,
            system_time: 132_000_000_000_000_000,
            server_start_time: 131_000_000_000_000_000,
            security_buffer: vec![0x60, 0x00],
            negotiate_contexts: vec![NegotiateContext::PreauthIntegrity {
                hash_algorithms: vec![HASH_ALGORITHM_SHA512],
                salt: vec![0xBB; 32],
            }],
        };
        pack_message(&h, &body)
    }

    /// Build a session setup response.
    fn build_session_setup_response(
        status: NtStatus,
        session_id: SessionId,
        security_buffer: Vec<u8>,
        session_flags: SessionFlags,
    ) -> Vec<u8> {
        let mut h = Header::new_request(Command::SessionSetup);
        h.flags.set_response();
        h.credits = 32;
        h.status = status;
        h.session_id = session_id;

        let body = SessionSetupResponse {
            session_flags,
            security_buffer,
        };

        pack_message(&h, &body)
    }

    /// Build a minimal NTLM challenge message (Type 2).
    fn build_ntlm_challenge() -> Vec<u8> {
        let mut buf = Vec::new();

        // Signature
        buf.extend_from_slice(b"NTLMSSP\0");
        // MessageType = 2
        buf.extend_from_slice(&2u32.to_le_bytes());
        // TargetNameFields: Len=0, MaxLen=0, Offset=56
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&56u32.to_le_bytes());
        // NegotiateFlags
        let flags: u32 = 0x0000_0001 // UNICODE
            | 0x0000_0200  // NTLM
            | 0x0008_0000  // EXTENDED_SESSIONSECURITY
            | 0x0080_0000  // TARGET_INFO
            | 0x2000_0000  // 128
            | 0x4000_0000  // KEY_EXCH
            | 0x8000_0000  // 56
            | 0x0000_0010  // SIGN
            | 0x0000_0020; // SEAL
        buf.extend_from_slice(&flags.to_le_bytes());
        // ServerChallenge
        buf.extend_from_slice(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF]);
        // Reserved
        buf.extend_from_slice(&[0u8; 8]);
        // TargetInfoFields
        let target_info = {
            let mut ti = Vec::new();
            ti.extend_from_slice(&0u16.to_le_bytes()); // MsvAvEOL AvId=0
            ti.extend_from_slice(&0u16.to_le_bytes()); // AvLen=0
            ti
        };
        let ti_offset = 56u32;
        buf.extend_from_slice(&(target_info.len() as u16).to_le_bytes());
        buf.extend_from_slice(&(target_info.len() as u16).to_le_bytes());
        buf.extend_from_slice(&ti_offset.to_le_bytes());
        while buf.len() < 56 {
            buf.push(0);
        }
        buf.extend_from_slice(&target_info);
        buf
    }

    /// Queue negotiate + session setup responses on a mock transport.
    fn queue_negotiate_and_session(mock: &MockTransport, session_id: SessionId) {
        mock.queue_response(build_negotiate_response());

        let challenge = build_ntlm_challenge();
        mock.queue_response(build_session_setup_response(
            NtStatus::MORE_PROCESSING_REQUIRED,
            session_id,
            challenge,
            SessionFlags(0),
        ));

        // A guest session, so signing stays off: every canned response below
        // arrives unsigned, and a signed session rejects those (MS-SMB2
        // § 3.2.5.1.3). These tests exercise routing, DFS, and reconnects,
        // not signing. The callers log in without a username to match: a
        // login naming an account refuses a guest session.
        mock.queue_response(build_session_setup_response(
            NtStatus::SUCCESS,
            session_id,
            vec![],
            SessionFlags(SessionFlags::IS_GUEST),
        ));
    }

    /// Create a mock-backed SmbClient without going through TCP.
    async fn make_mock_client(mock: &Arc<MockTransport>, session_id: SessionId) -> SmbClient {
        mock.enable_auto_rewrite_msg_id();
        queue_negotiate_and_session(mock, session_id);

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );

        conn.negotiate().await.unwrap();

        let session = Session::setup(&mut conn, "", "", "").await.unwrap();

        let config = ClientConfig {
            addr: "test-server:445".to_string(),
            timeout: Duration::from_secs(5),
            username: String::new(),
            password: String::new(),
            domain: String::new(),
            auto_reconnect: false,
            compression: true,
            dfs_enabled: true,
            dfs_target_overrides: std::collections::HashMap::new(),
            // These tests are about following; `dfs_consent` is about whether to.
            dfs_target_policy: DfsTargetPolicy::FollowAll,
            connect_options: None,
            ..Default::default()
        };

        SmbClient::from_parts(config, conn, session)
    }

    /// A TREE_CONNECT response for a share that demands encryption.
    fn build_encrypting_tree_connect_response(tree_id: TreeId) -> Vec<u8> {
        let mut h = Header::new_request(Command::TreeConnect);
        h.flags.set_response();
        h.credits = 32;
        h.tree_id = Some(tree_id);

        let body = crate::msg::tree_connect::TreeConnectResponse {
            share_type: ShareType::Disk,
            share_flags: crate::types::flags::ShareFlags::new(
                crate::types::flags::ShareFlags::ENCRYPT_DATA,
            ),
            capabilities: crate::types::flags::ShareCapabilities::default(),
            maximal_access: 0x001F_01FF,
        };
        pack_message(&h, &body)
    }

    /// A session with SMB 3.x encryption keys, which is what session setup
    /// produces on a dialect that supports encryption.
    fn session_with_keys(session_id: SessionId) -> Session {
        Session {
            session_id,
            signing_key: vec![0x11; 16],
            encryption_key: Some(vec![0x22; 16]),
            decryption_key: Some(vec![0x33; 16]),
            signing_algorithm: crate::crypto::signing::SigningAlgorithm::HmacSha256,
            should_sign: false,
            should_encrypt: false,
        }
    }

    /// Put an extra connection in the DFS pool, the way a redirect to another
    /// server does.
    fn pool_extra_connection(client: &mut SmbClient, addr: &str, mock: &Arc<MockTransport>) {
        let conn = crate::client::test_helpers::setup_connection(mock);
        client.connections.insert_extra(
            addr.to_string(),
            ConnectionEntry {
                conn,
                session: std::sync::Arc::new(session_with_keys(SessionId(0x99))),
            },
        );
    }

    fn a_dfs_target_tree(addr: &str, share: &str) -> Tree {
        Tree {
            pin: None,
            tree_id: TreeId(5),
            share_name: share.to_string(),
            server: addr.to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        }
    }

    /// `auto_reconnect` has to cover the connection the tree actually lives
    /// on. On a namespace root that is the *target* connection, carrying all
    /// of the user's traffic; asking the primary about it said "this is
    /// revivable" when it was not, and "this is not" when it was.
    #[tokio::test]
    async fn liveness_is_judged_on_the_trees_own_connection() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        client.config.auto_reconnect = true;
        // The primary is armed; the DFS target is not.
        client
            .connections
            .primary()
            .set_reviver(Some(Arc::new(MockReviver::new(SessionId(0x1)))));

        let target_mock = Arc::new(MockTransport::new());
        pool_extra_connection(&mut client, "dfs-target:445", &target_mock);

        let target_tree = a_dfs_target_tree("dfs-target:445", "secret");
        assert!(
            !client.session_is_gone(&target_tree, &Error::Disconnected),
            "the primary's reviver says nothing about the target connection"
        );

        // Arm the target, and it becomes recoverable.
        client
            .connections
            .extra("dfs-target:445")
            .unwrap()
            .conn
            .set_reviver(Some(Arc::new(MockReviver::new(SessionId(0x2)))));
        assert!(client.session_is_gone(&target_tree, &Error::Disconnected));

        // A tree on a server this client has no connection for is never
        // "recoverable", whatever the primary says.
        let stranger = a_dfs_target_tree("somewhere-else:445", "secret");
        assert!(!client.session_is_gone(&stranger, &Error::Disconnected));
    }

    /// Every `SmbClient` method that takes a `Tree` and hands back a handle or
    /// drives the transfer itself, one variant each. The rest go through the
    /// DFS retry wrappers and are covered by the same lookup.
    /// A checkpoint the resume openers will act on, so they get as far as
    /// sending their CREATE.
    fn resumable_checkpoint() -> stream::WriteCheckpoint {
        stream::WriteCheckpoint::new(
            10,
            Some(FileIdentity {
                volume_serial: 1,
                index_number: 1,
            }),
        )
    }

    #[derive(Debug, Clone, Copy)]
    enum TreeCall {
        Download,
        UploadSmall,
        UploadLarge,
        Watch,
        OpenFileReader,
        CreateFileWriter,
        CreateFileWriterExclusive,
        CreateFileWriterAt,
        ResumeFileWriter,
        WriteFileWithProgress,
    }

    impl TreeCall {
        const ALL: [TreeCall; 10] = [
            TreeCall::Download,
            TreeCall::UploadSmall,
            TreeCall::UploadLarge,
            TreeCall::Watch,
            TreeCall::OpenFileReader,
            TreeCall::CreateFileWriter,
            TreeCall::CreateFileWriterExclusive,
            TreeCall::CreateFileWriterAt,
            TreeCall::ResumeFileWriter,
            TreeCall::WriteFileWithProgress,
        ];

        /// Send the call's first request. Nothing answers it: only where it
        /// went matters, so the caller abandons the call after a moment.
        async fn start(self, client: &mut SmbClient, tree: &mut Tree) -> Result<()> {
            // Past the compound write limit, so the upload opens a handle.
            let large = vec![0u8; 200_000];
            match self {
                TreeCall::Download => client.download(tree, "f.bin").await.map(drop),
                TreeCall::UploadSmall => client.upload(tree, "f.bin", b"hi").await.map(drop),
                TreeCall::UploadLarge => client.upload(tree, "f.bin", &large).await.map(drop),
                TreeCall::Watch => client.watch(tree, "", false).await.map(drop),
                TreeCall::OpenFileReader => client.open_file_reader(tree, "f.bin").await.map(drop),
                TreeCall::CreateFileWriter => {
                    client.create_file_writer(tree, "f.bin").await.map(drop)
                }
                TreeCall::CreateFileWriterExclusive => client
                    .create_file_writer_exclusive(tree, "f.bin")
                    .await
                    .map(drop),
                TreeCall::CreateFileWriterAt => client
                    .create_file_writer_at(tree, "f.bin", 10)
                    .await
                    .map(drop),
                TreeCall::ResumeFileWriter => client
                    .resume_file_writer(tree, "f.bin", &resumable_checkpoint())
                    .await
                    .map(drop),
                TreeCall::WriteFileWithProgress => client
                    .write_file_with_progress(tree, "f.bin", b"hi", |_| ControlFlow::Continue(()))
                    .await
                    .map(drop),
            }
        }
    }

    /// A DFS-target tree's `TreeId` only means something to the session that
    /// issued it. Sent to the primary, it either fails with
    /// `STATUS_NETWORK_NAME_DELETED` or, when the ids collide, reads or writes
    /// a same-named file on a different share. So nothing may go out on the
    /// primary, and the request must go out on the target's own connection.
    #[tokio::test(start_paused = true)]
    async fn every_tree_call_goes_out_on_the_trees_own_connection() {
        let mut misrouted = Vec::new();
        for call in TreeCall::ALL {
            let primary = Arc::new(MockTransport::new());
            let mut client = make_mock_client(&primary, SessionId(0x77)).await;
            let target = Arc::new(MockTransport::new());
            pool_extra_connection(&mut client, "dfs-target:445", &target);
            let sent_on_primary = primary.sent_count();

            let mut tree = a_dfs_target_tree("dfs-target:445", "projects");
            let _ =
                tokio::time::timeout(Duration::from_secs(1), call.start(&mut client, &mut tree))
                    .await;

            if primary.sent_count() != sent_on_primary || target.sent_count() == 0 {
                misrouted.push(call);
            }
        }
        assert!(
            misrouted.is_empty(),
            "sent the target's tree over the primary connection: {misrouted:?}"
        );
    }

    /// A tree whose DFS connection is gone is refused before anything is sent,
    /// by the handle openers too, which have no `&mut self` to route with.
    #[tokio::test(start_paused = true)]
    async fn every_tree_call_on_an_orphaned_tree_says_disconnected() {
        let mut wrong = Vec::new();
        for call in TreeCall::ALL {
            let primary = Arc::new(MockTransport::new());
            let mut client = make_mock_client(&primary, SessionId(0x77)).await;
            let sent_on_primary = primary.sent_count();

            let mut orphan = a_dfs_target_tree("dfs-target:445", "projects");
            let outcome =
                tokio::time::timeout(Duration::from_secs(1), call.start(&mut client, &mut orphan))
                    .await;

            if !matches!(outcome, Ok(Err(Error::Disconnected)))
                || primary.sent_count() != sent_on_primary
            {
                wrong.push(call);
            }
        }
        assert!(
            wrong.is_empty(),
            "not refused as Disconnected before sending: {wrong:?}"
        );
    }

    // ── DFS links ─────────────────────────────────────────────────────

    /// The DFS root share the link tests start from, on the primary.
    fn a_dfs_root_tree() -> Tree {
        Tree {
            pin: None,
            tree_id: TreeId(1),
            share_name: "dfs".to_string(),
            server: "test-server:445".to_string(),
            is_dfs: true,
            encrypt_data: false,
            dfs_origin: None,
        }
    }

    /// The tree id the link's target share gets.
    const LINK_TARGET_TREE: TreeId = TreeId(7);

    /// An answer of `STATUS_PATH_NOT_COVERED` to a request of `ops` sub-requests,
    /// the way a server cascades the CREATE's failure through a compound.
    fn path_not_covered(ops: usize) -> Vec<u8> {
        let one =
            crate::client::test_helpers::build_create_error_response(NtStatus::PATH_NOT_COVERED);
        crate::client::test_helpers::build_compound_response_frame(&vec![one; ops])
    }

    /// What the server says when asked for the link's referral the first time:
    /// IPC$ for the IOCTL, the referral (`dfs\data` is `\\test-server\files`),
    /// and the target share's tree connect. The target is on the primary, so
    /// no second connection is dialled.
    fn queue_link_referral(mock: &MockTransport) {
        mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
        mock.queue_response(build_referral(
            3,
            0,
            r"\test-server\dfs\data",
            &[r"\test-server\files"],
            0,
        ));
        mock.queue_response(build_tree_connect_response(
            LINK_TARGET_TREE,
            ShareType::Disk,
        ));
    }

    /// `(tree id, command)` of the first sub-request in every frame sent.
    fn sent_requests(mock: &MockTransport) -> Vec<(Option<TreeId>, Command)> {
        mock.sent_messages()
            .iter()
            .filter_map(|bytes| Header::unpack(&mut crate::pack::ReadCursor::new(bytes)).ok())
            .map(|h| (h.tree_id, h.command))
            .collect()
    }

    fn creates_on(mock: &MockTransport) -> Vec<Option<TreeId>> {
        sent_requests(mock)
            .into_iter()
            .filter(|(_, command)| *command == Command::Create)
            .map(|(tree_id, _)| tree_id)
            .collect()
    }

    /// Every path-taking call that isn't a thin `&mut Tree` wrapper: the
    /// streaming, handle-opening, progress, and watch calls.
    #[derive(Debug, Clone, Copy)]
    enum LinkCall {
        Download,
        OpenFileDownload,
        UploadSmall,
        UploadLarge,
        Watch,
        OpenFileReader,
        CreateFileWriter,
        CreateFileWriterExclusive,
        CreateFileWriterAt,
        ResumeFileWriter,
        ReadFileWithProgress,
        WriteFileWithProgress,
        WriteFileStreamed,
    }

    impl LinkCall {
        const ALL: [LinkCall; 13] = [
            LinkCall::Download,
            LinkCall::OpenFileDownload,
            LinkCall::UploadSmall,
            LinkCall::UploadLarge,
            LinkCall::Watch,
            LinkCall::OpenFileReader,
            LinkCall::CreateFileWriter,
            LinkCall::CreateFileWriterExclusive,
            LinkCall::CreateFileWriterAt,
            LinkCall::ResumeFileWriter,
            LinkCall::ReadFileWithProgress,
            LinkCall::WriteFileWithProgress,
            LinkCall::WriteFileStreamed,
        ];

        /// How many sub-requests the call's first frame carries, so the refusal
        /// answers every one of them.
        fn first_frame_ops(self) -> usize {
            match self {
                // CREATE + WRITE + FLUSH (the CLOSE follows on its own).
                LinkCall::UploadSmall => 3,
                // CREATE + QUERY_INFO (class 18) + QUERY_INFO (class 48).
                LinkCall::OpenFileReader => 3,
                // CREATE + QUERY_INFO (index, volume, class 48).
                LinkCall::CreateFileWriter
                | LinkCall::CreateFileWriterExclusive
                | LinkCall::CreateFileWriterAt
                | LinkCall::ResumeFileWriter => 4,
                _ => 1,
            }
        }

        /// Run the call on `path`. Nothing answers the request after the
        /// redirect, so the caller abandons it after a moment; only where it
        /// went matters.
        async fn start(self, client: &mut SmbClient, tree: &mut Tree, path: &str) -> Result<()> {
            // Past the compound write limit, so the upload opens a handle.
            let large = vec![0u8; 200_000];
            let mut chunks = vec![b"hi".to_vec()].into_iter();
            let mut next = || chunks.next().map(Ok);
            match self {
                LinkCall::Download => client.download(tree, path).await.map(drop),
                LinkCall::OpenFileDownload => client.open_file_download(tree, path).await.map(drop),
                LinkCall::UploadSmall => client.upload(tree, path, b"hi").await.map(drop),
                LinkCall::UploadLarge => client.upload(tree, path, &large).await.map(drop),
                LinkCall::Watch => client.watch(tree, path, false).await.map(drop),
                LinkCall::OpenFileReader => client.open_file_reader(tree, path).await.map(drop),
                LinkCall::CreateFileWriter => client.create_file_writer(tree, path).await.map(drop),
                LinkCall::CreateFileWriterExclusive => client
                    .create_file_writer_exclusive(tree, path)
                    .await
                    .map(drop),
                LinkCall::CreateFileWriterAt => {
                    client.create_file_writer_at(tree, path, 10).await.map(drop)
                }
                LinkCall::ResumeFileWriter => client
                    .resume_file_writer(tree, path, &resumable_checkpoint())
                    .await
                    .map(drop),
                LinkCall::ReadFileWithProgress => client
                    .read_file_with_progress(tree, path, |_| ControlFlow::Continue(()))
                    .await
                    .map(drop),
                LinkCall::WriteFileWithProgress => client
                    .write_file_with_progress(tree, path, b"hi", |_| ControlFlow::Continue(()))
                    .await
                    .map(drop),
                LinkCall::WriteFileStreamed => client
                    .write_file_streamed(tree, path, &mut next)
                    .await
                    .map(drop),
            }
        }
    }

    /// A link inside a share answers the CREATE with `STATUS_PATH_NOT_COVERED`.
    /// Every call follows it once, the same as the `&mut Tree` wrappers, so a
    /// consumer copying or watching a folder behind a link never has to know
    /// which calls follow links.
    #[tokio::test(start_paused = true)]
    async fn every_path_call_follows_a_dfs_link() {
        let mut stayed = Vec::new();
        for call in LinkCall::ALL {
            let mock = Arc::new(MockTransport::new());
            let mut client = make_mock_client(&mock, SessionId(0x77)).await;
            mock.queue_response(path_not_covered(call.first_frame_ops()));
            queue_link_referral(&mock);

            let mut tree = a_dfs_root_tree();
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                call.start(&mut client, &mut tree, "data/f.bin"),
            )
            .await;

            if creates_on(&mock) != [Some(TreeId(1)), Some(LINK_TARGET_TREE)] {
                stayed.push((call, creates_on(&mock)));
            }
        }
        assert!(
            stayed.is_empty(),
            "didn't retry once on the link's target: {stayed:?}"
        );
    }

    /// A link that points at another link is followed once, not forever: the
    /// second `STATUS_PATH_NOT_COVERED` is the caller's answer.
    #[tokio::test(start_paused = true)]
    async fn a_link_is_followed_only_once() {
        for call in [
            LinkCall::Download,
            LinkCall::OpenFileDownload,
            LinkCall::CreateFileWriter,
        ] {
            let mock = Arc::new(MockTransport::new());
            let mut client = make_mock_client(&mock, SessionId(0x77)).await;
            mock.queue_response(path_not_covered(call.first_frame_ops()));
            queue_link_referral(&mock);
            mock.queue_response(path_not_covered(call.first_frame_ops()));

            let mut tree = a_dfs_root_tree();
            let outcome = tokio::time::timeout(
                Duration::from_secs(1),
                call.start(&mut client, &mut tree, "data/f.bin"),
            )
            .await;

            assert!(
                matches!(
                    outcome,
                    Ok(Err(Error::Protocol {
                        status: NtStatus::PATH_NOT_COVERED,
                        ..
                    }))
                ),
                "{call:?}: {outcome:?}"
            );
            assert_eq!(creates_on(&mock).len(), 2, "{call:?}");
        }
    }

    /// A referral that can't be had leaves the caller with the server's own
    /// answer, never the referral's: the IOCTL's failure says nothing about the
    /// path the caller asked for.
    #[tokio::test(start_paused = true)]
    async fn a_failed_link_referral_hands_back_the_original_error() {
        for call in [
            LinkCall::Download,
            LinkCall::OpenFileDownload,
            LinkCall::CreateFileWriter,
        ] {
            let mock = Arc::new(MockTransport::new());
            let mut client = make_mock_client(&mock, SessionId(0x77)).await;
            mock.queue_response(path_not_covered(call.first_frame_ops()));
            mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
            mock.queue_response(build_ioctl_refusal(NtStatus::NOT_FOUND));

            let mut tree = a_dfs_root_tree();
            let outcome = tokio::time::timeout(
                Duration::from_secs(1),
                call.start(&mut client, &mut tree, "data/f.bin"),
            )
            .await;

            assert!(
                matches!(
                    outcome,
                    Ok(Err(Error::Protocol {
                        status: NtStatus::PATH_NOT_COVERED,
                        command: Command::Create,
                    }))
                ),
                "{call:?}: {outcome:?}"
            );
        }

        // The `&mut Tree` wrappers too.
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        mock.queue_response(path_not_covered(1));
        mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
        mock.queue_response(build_ioctl_refusal(NtStatus::NOT_FOUND));
        let mut tree = a_dfs_root_tree();
        let outcome = client.list_directory(&mut tree, "data").await;
        assert!(
            matches!(
                outcome,
                Err(Error::Protocol {
                    status: NtStatus::PATH_NOT_COVERED,
                    command: Command::Create,
                })
            ),
            "{outcome:?}"
        );
        assert_eq!(tree.tree_id, TreeId(1), "a failed referral moved the tree");
    }

    /// A delete compound (CREATE + SET_INFO + CLOSE) that succeeded.
    fn deleted() -> Vec<u8> {
        use crate::client::test_helpers::*;
        build_compound_response_frame(&[
            build_create_response(FileId::SENTINEL, 0),
            build_set_info_response(),
            build_close_response(),
        ])
    }

    /// A batch follows a link for the items behind it, asking for the referral
    /// and connecting the target share once, and keeps every other item, and
    /// the caller's tree, on the share it named. Only the first linked item
    /// is refused: the rest go straight to the target from the cache.
    #[tokio::test(start_paused = true)]
    async fn a_batch_follows_a_link_once_and_keeps_the_rest_on_its_share() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        // data/a: refused, referral, target share, retried there.
        mock.queue_response(path_not_covered(3));
        queue_link_referral(&mock);
        mock.queue_response(deleted());
        // data/b: straight to the target, the cache knows the link.
        mock.queue_response(deleted());
        // plain.txt: an ordinary file in the root share.
        mock.queue_response(deleted());

        let tree = a_dfs_root_tree();
        let results = client
            .delete_files(&tree, &["data/a", "data/b", "plain.txt"])
            .await;

        assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
        assert_eq!(
            creates_on(&mock),
            [
                Some(TreeId(1)),
                Some(LINK_TARGET_TREE),
                Some(LINK_TARGET_TREE),
                Some(TreeId(1)),
            ]
        );
        let sent = sent_requests(&mock);
        let count = |command| sent.iter().filter(|(_, c)| *c == command).count();
        assert_eq!(count(Command::Ioctl), 1, "one referral for the whole batch");
        assert_eq!(
            count(Command::TreeConnect),
            2,
            "IPC$ and the target share, once each"
        );
        assert_eq!(tree.tree_id, TreeId(1), "the caller's tree stays put");
        mock.assert_fully_consumed();
    }

    /// Every call after the first through a link skips the refused CREATE,
    /// the single-file ones included (MS-DFSC § 3.1.4.1's cache check).
    #[tokio::test(start_paused = true)]
    async fn a_cached_link_sends_no_refused_create() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        mock.queue_response(path_not_covered(3));
        queue_link_referral(&mock);
        mock.queue_response(deleted());
        mock.queue_response(deleted());
        mock.queue_response(deleted());

        let tree = a_dfs_root_tree();
        for path in ["data/a", "DATA/sub/b", "data/c"] {
            client.delete_file(&tree, path).await.expect(path);
        }

        assert_eq!(
            creates_on(&mock),
            [
                Some(TreeId(1)),
                Some(LINK_TARGET_TREE),
                Some(LINK_TARGET_TREE),
                Some(LINK_TARGET_TREE),
            ]
        );
        assert_eq!(client.diagnostics().client.metrics.dfs_cache_hits, 2);
        mock.assert_fully_consumed();
    }

    /// A move between folders inside one link renames on the link's target,
    /// with the destination moved onto the target too.
    #[tokio::test(start_paused = true)]
    async fn a_move_across_folders_inside_a_link_renames_on_its_target() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        mock.queue_response(path_not_covered(3));
        queue_link_referral(&mock);
        mock.queue_response(deleted());

        let tree = a_dfs_root_tree();
        client
            .rename(&tree, "data/a/x.txt", "data/b/x.txt")
            .await
            .expect("a move inside the link");

        assert_eq!(creates_on(&mock), [Some(TreeId(1)), Some(LINK_TARGET_TREE)]);
        let rename = mock.sent_messages().last().cloned().expect("the rename");
        let to: Vec<u8> = "b\\x.txt"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(
            rename.windows(to.len()).any(|w| w == to.as_slice()),
            "the destination isn't the target's b\\x.txt"
        );
        mock.assert_fully_consumed();
    }

    /// A move out of a link would be a move to another share, which no
    /// single rename does: the server's answer stands, and nothing is renamed
    /// on the target.
    #[tokio::test(start_paused = true)]
    async fn a_move_out_of_a_link_keeps_the_servers_answer() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        mock.queue_response(path_not_covered(3));
        queue_link_referral(&mock);

        let tree = a_dfs_root_tree();
        let outcome = client.rename(&tree, "data/x.txt", "x.txt").await;

        assert!(
            matches!(
                outcome,
                Err(Error::Protocol {
                    status: NtStatus::PATH_NOT_COVERED,
                    command: Command::Create,
                })
            ),
            "{outcome:?}"
        );
        assert_eq!(creates_on(&mock), [Some(TreeId(1))]);
        mock.assert_fully_consumed();
    }

    /// `resolve_dfs_link` names the link's target tree and the path on it,
    /// asks the server once, and answers from the cache after that.
    #[tokio::test(start_paused = true)]
    async fn resolve_dfs_link_finds_the_target_and_caches_it() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        queue_link_referral(&mock);
        let tree = a_dfs_root_tree();

        let link = client
            .resolve_dfs_link(&tree, "data/Sub/f.bin")
            .await
            .expect("resolve")
            .expect("behind the link");
        assert_eq!(link.tree.tree_id, LINK_TARGET_TREE);
        assert_eq!(link.tree.share_name, "files");
        assert_eq!(link.path, "Sub/f.bin");
        assert_eq!(link.link_path, "data");

        let sent = mock.sent_messages().len();
        let again = client
            .resolve_dfs_link(&tree, "DATA/other")
            .await
            .expect("resolve")
            .expect("still behind the link");
        assert_eq!(
            (again.tree.tree_id, again.path.as_str()),
            (LINK_TARGET_TREE, "other")
        );
        assert_eq!(again.link_path, "DATA");
        assert_eq!(mock.sent_messages().len(), sent, "the cache answered");

        // A call through the link now goes straight to the target.
        mock.queue_response(deleted());
        client.delete_file(&tree, "data/x").await.expect("delete");
        assert_eq!(creates_on(&mock), [Some(LINK_TARGET_TREE)]);
        assert_eq!(tree.tree_id, TreeId(1), "the caller's tree stays put");
        mock.assert_fully_consumed();
    }

    /// A link whose targets all refuse is the link's problem: the connection
    /// to the caller's share is fine, so it must not read as a lost one
    /// (vdavid/smb2#28). `resolve_dfs_link` and a followed call say the same.
    #[tokio::test(start_paused = true)]
    async fn a_link_with_no_reachable_target_is_not_a_lost_connection() {
        use crate::ErrorKind;
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        let tree = a_dfs_root_tree();

        mock.queue_response(path_not_covered(3));
        mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
        mock.queue_response(build_referral(
            3,
            0,
            r"\test-server\dfs\data",
            &[r"\test-server\files"],
            0,
        ));
        mock.queue_response(build_tree_connect_refusal(NtStatus::ACCESS_DENIED, false));
        let err = client.delete_file(&tree, "data/a").await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DfsTargetUnreachable);
        assert!(err.is_retryable(), "the target may be back later");
        match err {
            Error::DfsTargetUnreachable {
                path,
                target_count,
                source,
            } => {
                assert_eq!(path, "data/a");
                assert_eq!(target_count, 1);
                assert_eq!(source.kind(), ErrorKind::AccessDenied);
            }
            other => panic!("expected DfsTargetUnreachable, got {other:?}"),
        }

        mock.queue_response(build_tree_connect_refusal(NtStatus::ACCESS_DENIED, false));
        let err = client
            .resolve_dfs_link(&tree, "data/b")
            .await
            .expect_err("no target answers");
        assert_eq!(err.kind(), ErrorKind::DfsTargetUnreachable);
        mock.assert_fully_consumed();
    }

    /// A namespace root is the share itself: nothing else is connected, so
    /// its dead storage keeps reading as a lost connection.
    #[test]
    fn a_namespace_with_no_reachable_target_stays_a_lost_connection() {
        let err = Error::DfsNoReachableTarget {
            namespace: r"\\dom\ns".to_string(),
            target_count: 1,
            source: Box::new(Error::Disconnected),
        };
        assert_eq!(err.kind(), crate::ErrorKind::ConnectionLost);
    }

    /// A link that points at the share's own server, with that connection
    /// gone: this IS the lost connection, and saying "the link's target" would
    /// hide it from whoever reconnects on `ConnectionLost`.
    #[tokio::test(start_paused = true)]
    async fn a_link_on_the_shares_own_dead_connection_says_the_connection_is_lost() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        let tree = a_dfs_root_tree();
        queue_link_referral(&mock);
        client
            .resolve_dfs_link(&tree, "data/a")
            .await
            .expect("resolve")
            .expect("behind the link");

        client.connections.primary().mark_dead();

        let err = client
            .resolve_dfs_link(&tree, "data/a")
            .await
            .expect_err("the connection is gone");
        assert_eq!(err.kind(), crate::ErrorKind::ConnectionLost);
    }

    /// A target's connection dying under a call through a link is the link's
    /// loss too. The pool lets go of it, so the next call dials afresh rather
    /// than failing on the dead connection for the rest of the client's life.
    #[tokio::test(start_paused = true)]
    async fn a_lost_target_connection_is_the_links_and_leaves_the_pool() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        let target_mock = Arc::new(MockTransport::new());
        pool_extra_connection(&mut client, "fs02:445", &target_mock);
        let target = a_dfs_target_tree("fs02:445", "files");
        client.connections.remember_link_tree(&target);

        client
            .connections
            .for_tree_ref(&target)
            .unwrap()
            .mark_dead();
        let err = client.delete_file(&target, "a").await.unwrap_err();

        assert_eq!(err.kind(), crate::ErrorKind::DfsTargetUnreachable);
        assert!(client.connections.extra("fs02:445").is_none());
        assert!(client.connections.link_tree("fs02:445", "files").is_none());
    }

    /// A pooled target whose connection died while idle isn't handed out, so
    /// the next lookup connects again.
    #[tokio::test(start_paused = true)]
    async fn a_dead_pooled_target_tree_is_not_handed_out() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        let target_mock = Arc::new(MockTransport::new());
        pool_extra_connection(&mut client, "fs02:445", &target_mock);
        let target = a_dfs_target_tree("fs02:445", "files");
        client.connections.remember_link_tree(&target);
        assert!(client.connections.link_tree("fs02:445", "files").is_some());

        client
            .connections
            .for_tree_ref(&target)
            .unwrap()
            .mark_dead();

        assert!(client.connections.link_tree("fs02:445", "files").is_none());
    }

    /// A tree that isn't a link's target (a namespace root's, here) keeps its
    /// lost connection: it's the share itself.
    #[tokio::test(start_paused = true)]
    async fn a_lost_connection_on_a_namespace_target_stays_a_lost_connection() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        let target_mock = Arc::new(MockTransport::new());
        pool_extra_connection(&mut client, "fs02:445", &target_mock);
        let target = a_dfs_target_tree("fs02:445", "files");

        client
            .connections
            .for_tree_ref(&target)
            .unwrap()
            .mark_dead();
        let err = client.delete_file(&target, "a").await.unwrap_err();

        assert_eq!(err.kind(), crate::ErrorKind::ConnectionLost);
        assert!(client.connections.extra("fs02:445").is_some());
    }

    /// A path no link covers is `None`: for free on a share that isn't in a
    /// DFS tree or at the share's root, and from the server's refusal on one
    /// that is.
    #[tokio::test(start_paused = true)]
    async fn resolve_dfs_link_is_none_outside_a_link() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        let sent = mock.sent_messages().len();

        let plain = Tree {
            is_dfs: false,
            ..a_dfs_root_tree()
        };
        assert!(client
            .resolve_dfs_link(&plain, "data/f")
            .await
            .unwrap()
            .is_none());
        let tree = a_dfs_root_tree();
        assert!(client.resolve_dfs_link(&tree, "/").await.unwrap().is_none());
        assert_eq!(mock.sent_messages().len(), sent, "nothing on the wire");

        // What Samba answers for a path outside every link.
        mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
        mock.queue_response(build_ioctl_refusal(NtStatus::OBJECT_PATH_NOT_FOUND));
        assert!(client
            .resolve_dfs_link(&tree, "plain.txt")
            .await
            .unwrap()
            .is_none());
        mock.assert_fully_consumed();
    }

    /// A single-file call through a link leaves the caller's tree on its own
    /// share, so the next path on it still means what the caller meant. When
    /// the tree moved, `plain.txt` went to the link's target share: a
    /// `NOT_FOUND`, or a same-named file there read or deleted.
    #[tokio::test(start_paused = true)]
    async fn a_call_through_a_link_leaves_the_callers_tree_on_its_share() {
        use crate::client::test_helpers::*;
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        // data/a: refused, referral, target share, deleted there.
        mock.queue_response(path_not_covered(3));
        queue_link_referral(&mock);
        mock.queue_response(deleted());
        // plain.txt, read from the root share: CREATE + READ + CLOSE.
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FileId::SENTINEL, 5),
            build_read_response(b"plain".to_vec()),
            build_close_response(),
        ]));

        let mut tree = a_dfs_root_tree();
        client
            .delete_file(&tree, "data/a")
            .await
            .expect("delete through the link");
        let plain = client
            .read_file(&mut tree, "plain.txt")
            .await
            .expect("read from the root share");

        assert_eq!(plain, b"plain");
        assert_eq!(
            creates_on(&mock),
            [Some(TreeId(1)), Some(LINK_TARGET_TREE), Some(TreeId(1))]
        );
        assert_eq!((tree.tree_id, tree.share_name.as_str()), (TreeId(1), "dfs"));
        mock.assert_fully_consumed();
    }

    /// A path the link's target hands back reads in the caller's terms: the
    /// caller's link folder, then the target path past the target's own
    /// folder, so it opens the same file on the caller's tree.
    #[test]
    fn a_target_path_maps_back_through_the_link() {
        // `data` -> `\\fs02\files`: data/HELLO.TXT is hello.txt there.
        let link = Link::new(a_dfs_root_tree(), "data/HELLO.TXT", "HELLO.TXT", 1);
        assert_eq!(link.caller_path("hello.txt"), "data/hello.txt");
        // `a/data` -> `\\fs02\files\sub`: a/data/x/f is sub/x/f there.
        let link = Link::new(a_dfs_root_tree(), "a/data/X/f", "sub/X/f", 2);
        assert_eq!(link.caller_path("sub/x/f"), "a/data/x/f");
        assert_eq!(link.link_path(), "a/data");
        // The link folder itself.
        let link = Link::new(a_dfs_root_tree(), "data", "", 1);
        assert_eq!(link.caller_path(""), "data");
    }

    /// A rename's destination is share-relative, so behind a link it has to
    /// move with the source: anywhere inside the link folder maps onto the
    /// target, across subfolders too, and a path outside it (another share,
    /// as far as the server is concerned) or the link folder itself doesn't.
    #[test]
    fn a_path_inside_the_link_maps_onto_its_target() {
        let link = Link::new(a_dfs_root_tree(), "data/a/x.txt", "a/x.txt", 1);
        assert_eq!(link.target_path("data/b/x.txt").as_deref(), Some("b/x.txt"));
        assert_eq!(link.target_path("/DATA/y.txt/").as_deref(), Some("y.txt"));
        assert_eq!(link.target_path("b/x.txt"), None, "outside the link");
        assert_eq!(link.target_path("database/x"), None, "a whole component");
        assert_eq!(link.target_path("data"), None, "the link folder itself");

        // A link to a folder inside its target share keeps that folder.
        let link = Link::new(a_dfs_root_tree(), "p/data/a/x", "deep/a/x", 2);
        assert_eq!(link.target_path("p/data/b/x").as_deref(), Some("deep/b/x"));
        assert_eq!(link.target_path("data/b/x"), None);
    }

    /// Consumers spawn these on multi-threaded runtimes, so following a link
    /// must not cost the futures their `Send`.
    #[allow(dead_code, clippy::let_underscore_future)]
    fn dfs_following_futures_stay_send(client: &mut SmbClient, tree: &mut Tree) {
        fn send<T: Send>(_: T) {}
        send(client.download(tree, "f"));
        send(client.open_file_download(tree, "f"));
        send(client.watch(tree, "f", false));
        send(client.open_directory_reader(tree, "f"));
        send(client.list_directory_with(tree, "f", ListOptions::new()));
        send(client.create_file_writer(tree, "f"));
        send(client.resume_file_writer(tree, "f", &resumable_checkpoint()));
        send(client.delete_files(tree, &["f"]));
        send(client.read_file_with_progress(tree, "f", |_| ControlFlow::Continue(())));
    }

    /// `open_file_download`'s download borrows nothing, so a consumer can hold
    /// the client for the open alone and spawn the download onto its own task.
    #[allow(dead_code, clippy::let_underscore_future)]
    fn an_unborrowed_download_is_static_and_send(download: FileDownload<'static>) {
        fn spawnable<T: Send + 'static>(_: T) {}
        spawnable(download);
    }

    /// The download `open_file_download` hands back outlives the borrow of the
    /// client it was opened through, and reads from the link's target with
    /// the adaptive read-ahead window `Tree::download` uses.
    #[tokio::test(start_paused = true)]
    async fn an_unborrowed_download_through_a_link_outlives_the_client_borrow() {
        use crate::client::test_helpers::*;
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        mock.queue_response(path_not_covered(1));
        queue_link_referral(&mock);
        mock.queue_response(build_create_response(FileId::SENTINEL, 0));

        let tree = a_dfs_root_tree();
        let download = client
            .open_file_download(&tree, "data/f.bin")
            .await
            .expect("open through the link");
        // The client is free again while the download lives.
        drop(client);

        assert!(matches!(download.read_ahead(), crate::ReadAhead::Adaptive));
        assert_eq!(download.size(), 0);
        assert_eq!(creates_on(&mock), [Some(TreeId(1)), Some(LINK_TARGET_TREE)]);
    }

    // ── DFS namespace roots ───────────────────────────────────────────

    /// A TREE_CONNECT refusal, optionally carrying the cluster-redirect error
    /// context that reuses the same status code.
    fn build_tree_connect_refusal(status: NtStatus, share_redirect: bool) -> Vec<u8> {
        let mut h = Header::new_request(Command::TreeConnect);
        h.flags.set_response();
        h.credits = 32;
        h.status = status;

        let (error_context_count, error_data) = if share_redirect {
            // One SMB2 ERROR Context: ErrorDataLength(4) + ErrorId(4) + a
            // Share Redirect body we never look inside.
            let body = vec![0u8; 48];
            let mut data = Vec::new();
            data.extend_from_slice(&(body.len() as u32).to_le_bytes());
            data.extend_from_slice(&0x7264_5253u32.to_le_bytes());
            data.extend_from_slice(&body);
            (1u8, data)
        } else {
            (0u8, Vec::new())
        };

        pack_message(
            &h,
            &crate::msg::header::ErrorResponse {
                error_context_count,
                error_data,
            },
        )
    }

    /// A Samba-shaped V3 root referral: `ServerType = 0`.
    fn build_root_referral(namespace: &str, targets: &[&str], header_flags: u32) -> Vec<u8> {
        build_referral(3, 0, namespace, targets, header_flags)
    }

    /// A referral at a chosen version and `ServerType`, wrapped in an IOCTL
    /// response.
    fn build_referral(
        version: u16,
        server_type: u16,
        namespace: &str,
        targets: &[&str],
        header_flags: u32,
    ) -> Vec<u8> {
        let entries: Vec<(&str, &str, &str, u32)> = targets
            .iter()
            .map(|t| (namespace, namespace, *t, 600u32))
            .collect();
        let payload = crate::client::dfs::tests::pack_referral(
            version,
            server_type,
            (namespace.encode_utf16().count() * 2) as u16,
            header_flags,
            &entries,
        );

        let mut h = Header::new_request(Command::Ioctl);
        h.flags.set_response();
        h.credits = 32;
        pack_message(
            &h,
            &crate::msg::ioctl::IoctlResponse {
                ctl_code: crate::msg::ioctl::FSCTL_DFS_GET_REFERRALS,
                file_id: FileId::SENTINEL,
                flags: crate::msg::ioctl::SMB2_0_IOCTL_IS_FSCTL,
                output_data: payload,
            },
        )
    }

    fn build_ioctl_refusal(status: NtStatus) -> Vec<u8> {
        let mut h = Header::new_request(Command::Ioctl);
        h.flags.set_response();
        h.credits = 32;
        h.status = status;
        pack_message(
            &h,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        )
    }

    /// The headline: a namespace root is not a share on the server answering
    /// its name, so `TREE_CONNECT` is refused and only a referral can say
    /// where the storage actually is. Before this, the path could not be
    /// opened at all.
    #[tokio::test]
    async fn a_namespace_root_resolves_to_the_target_the_referral_names() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;

        // The share the caller named isn't one: STATUS_BAD_NETWORK_NAME.
        mock.queue_response(build_tree_connect_refusal(
            NtStatus::BAD_NETWORK_NAME,
            false,
        ));
        // IPC$ for the referral, then the referral itself. Samba's shape:
        // ServerType 0 and StorageServers alone.
        mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
        mock.queue_response(build_root_referral(
            r"\test-server\projects",
            &[r"\test-server\projects_dfs"],
            0x02,
        ));
        // And the tree connect to the target share.
        mock.queue_response(build_tree_connect_response(TreeId(9), ShareType::Disk));

        let tree = client
            .connect_share("projects")
            .await
            .expect("a namespace root must resolve");

        assert_eq!(tree.tree_id, TreeId(9));
        assert_eq!(tree.share_name, "projects_dfs");
        assert_eq!(
            tree.dfs_origin,
            Some(DfsOrigin {
                requested: r"\\test-server\projects".to_string(),
                target: r"\\test-server\projects_dfs".to_string(),
            }),
            "the caller's own name for the namespace has to survive the redirect"
        );
    }

    /// Windows and Samba answer the *same* root referral differently, and both
    /// must resolve identically.
    ///
    /// Windows sets `ServerType = 1` and both header bits (MS-DFSC § 4.3,
    /// § 4.6); Samba answers `ServerType = 0` and StorageServers alone
    /// (verified against Samba 4.20.6, 2026-09-16). ❌ Nothing may gate on
    /// `ServerType`: a client that requires 1 works against Windows and
    /// silently refuses every Samba namespace. The Docker fixtures only ever
    /// produce the Samba shape, so this is the side they cannot cover.
    #[tokio::test]
    async fn windows_and_samba_shaped_root_referrals_resolve_the_same() {
        // (version, server_type, header_flags)
        let shapes = [
            (4u16, 1u16, 0x03u32, "Windows"),
            (3, 0, 0x02, "Samba"),
            // A Windows-shaped V3, which § 4.3's text also allows.
            (3, 1, 0x03, "Windows V3"),
        ];

        for (version, server_type, header_flags, who) in shapes {
            let mock = Arc::new(MockTransport::new());
            let mut client = make_mock_client(&mock, SessionId(0x77)).await;

            mock.queue_response(build_tree_connect_refusal(
                NtStatus::BAD_NETWORK_NAME,
                false,
            ));
            mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
            mock.queue_response(build_referral(
                version,
                server_type,
                r"\test-server\projects",
                &[r"\test-server\projects_dfs"],
                header_flags,
            ));
            mock.queue_response(build_tree_connect_response(TreeId(9), ShareType::Disk));

            let tree = client
                .connect_share("projects")
                .await
                .unwrap_or_else(|e| panic!("a {who}-shaped root referral must resolve: {e}"));
            assert_eq!(tree.share_name, "projects_dfs", "{who}");
            assert_eq!(
                tree.dfs_origin.as_ref().map(|o| o.requested.as_str()),
                Some(r"\\test-server\projects"),
                "{who}"
            );
        }
    }

    /// ❌ A failed referral must not replace the original error. The
    /// overwhelmingly common cause of `STATUS_BAD_NETWORK_NAME` is a typo, and
    /// telling that person about DFS is worse than telling them nothing.
    #[tokio::test]
    async fn a_mistyped_share_name_still_reports_the_missing_share() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;

        mock.queue_response(build_tree_connect_refusal(
            NtStatus::BAD_NETWORK_NAME,
            false,
        ));
        mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
        // No namespace by that name either.
        mock.queue_response(build_ioctl_refusal(NtStatus::NOT_FOUND));

        let err = client.connect_share("shrae").await.unwrap_err();
        assert!(
            matches!(
                err,
                Error::Protocol {
                    status: NtStatus::BAD_NETWORK_NAME,
                    command: Command::TreeConnect
                }
            ),
            "expected the original refusal, got {err:?}"
        );
    }

    /// A referral that answers with no targets at all is the same story.
    #[tokio::test]
    async fn an_empty_referral_reports_the_missing_share() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;

        mock.queue_response(build_tree_connect_refusal(
            NtStatus::BAD_NETWORK_NAME,
            false,
        ));
        mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
        mock.queue_response(build_root_referral(r"\test-server\projects", &[], 0x02));

        let err = client.connect_share("projects").await.unwrap_err();
        assert!(matches!(
            err,
            Error::Protocol {
                status: NtStatus::BAD_NETWORK_NAME,
                ..
            }
        ));
    }

    /// A chain that ends empty is a chain that ended, not a hop limit that was
    /// reached. The caller gets the original refusal, and above all not a
    /// `DfsTooManyReferrals` naming a limit nothing came near.
    #[tokio::test]
    async fn an_empty_referral_after_an_interlink_is_not_a_hop_limit_error() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;

        mock.queue_response(build_tree_connect_refusal(
            NtStatus::BAD_NETWORK_NAME,
            false,
        ));
        mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
        // Hop 1: an interlink into a second namespace.
        mock.queue_response(build_root_referral(
            r"\test-server\projects",
            &[r"\test-server\elsewhere"],
            0x01,
        ));
        // Hop 2: nothing there.
        mock.queue_response(build_root_referral(r"\test-server\elsewhere", &[], 0x02));

        let err = client.connect_share("projects").await.unwrap_err();
        assert!(
            matches!(
                err,
                Error::Protocol {
                    status: NtStatus::BAD_NETWORK_NAME,
                    ..
                }
            ),
            "the chain ended after two hops, so the caller should get the \
             original refusal; got {err:?}"
        );
    }

    /// The one outcome that is genuinely about DFS: the namespace is real, we
    /// found it, and its storage is out of reach. No other error says that.
    #[tokio::test]
    async fn a_namespace_whose_targets_are_all_down_says_exactly_that() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;

        mock.queue_response(build_tree_connect_refusal(
            NtStatus::BAD_NETWORK_NAME,
            false,
        ));
        mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
        mock.queue_response(build_root_referral(
            r"\test-server\projects",
            &[r"\test-server\one", r"\test-server\two"],
            0x02,
        ));
        // Both targets refuse.
        mock.queue_response(build_tree_connect_refusal(NtStatus::ACCESS_DENIED, false));
        mock.queue_response(build_tree_connect_refusal(NtStatus::ACCESS_DENIED, false));

        let err = client.connect_share("projects").await.unwrap_err();
        match err {
            Error::DfsNoReachableTarget {
                namespace,
                target_count,
                ..
            } => {
                assert_eq!(namespace, r"\\test-server\projects");
                assert_eq!(target_count, 2);
            }
            other => panic!("expected DfsNoReachableTarget, got {other:?}"),
        }
    }

    /// § 3.1.4.1 step 2: the second connect goes straight to the target, so
    /// the refused TREE_CONNECT and the referral are paid once per TTL rather
    /// than once per connect.
    #[tokio::test]
    async fn a_known_namespace_skips_the_refused_tree_connect() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;

        mock.queue_response(build_tree_connect_refusal(
            NtStatus::BAD_NETWORK_NAME,
            false,
        ));
        mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
        mock.queue_response(build_root_referral(
            r"\test-server\projects",
            &[r"\test-server\projects_dfs"],
            0x02,
        ));
        mock.queue_response(build_tree_connect_response(TreeId(9), ShareType::Disk));

        client.connect_share("projects").await.unwrap();
        let after_first = mock.sent_count();

        mock.queue_response(build_tree_connect_response(TreeId(10), ShareType::Disk));
        let tree = client.connect_share("projects").await.unwrap();

        assert_eq!(tree.tree_id, TreeId(10));
        assert!(tree.dfs_origin.is_some());
        assert_eq!(
            mock.sent_count() - after_first,
            1,
            "a cached namespace should cost one tree connect, nothing else"
        );
    }

    /// `STATUS_BAD_NETWORK_NAME` with an `SMB2_ERROR_ID_SHARE_REDIRECT`
    /// context is scale-out cluster redirection (MS-SMB2 § 2.2.2.2.2), an
    /// unrelated mechanism reusing the same status. Chasing a DFS referral
    /// for it would be nonsense, so it never gets one.
    #[tokio::test]
    async fn a_cluster_redirect_is_not_mistaken_for_a_namespace() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;

        mock.queue_response(build_tree_connect_refusal(NtStatus::BAD_NETWORK_NAME, true));
        let before = mock.sent_count();

        let err = client.connect_share("clustered").await.unwrap_err();
        assert!(
            matches!(&err, Error::ShareRedirected { share } if share == "clustered"),
            "expected ShareRedirected, got {err:?}"
        );
        assert_eq!(
            mock.sent_count() - before,
            1,
            "no referral should be sent for a cluster redirect"
        );
    }

    /// A server that never advertised `SMB2_GLOBAL_CAP_DFS` is not asked for
    /// a referral, so a plain NAS pays nothing when someone mistypes a share.
    #[tokio::test]
    async fn a_server_without_cap_dfs_is_never_asked_for_a_referral() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;
        client
            .connections
            .primary_mut()
            .set_test_params(NegotiatedParams {
                dialect: Dialect::Smb3_1_1,
                max_read_size: 65536,
                max_write_size: 65536,
                max_transact_size: 65536,
                server_guid: Guid::ZERO,
                signing_required: false,
                capabilities: Capabilities::default(), // no DFS
                gmac_negotiated: false,
                cipher: None,
                compression_supported: false,
            });

        mock.queue_response(build_tree_connect_refusal(
            NtStatus::BAD_NETWORK_NAME,
            false,
        ));
        let before = mock.sent_count();

        let err = client.connect_share("projects").await.unwrap_err();
        assert!(matches!(
            err,
            Error::Protocol {
                status: NtStatus::BAD_NETWORK_NAME,
                ..
            }
        ));
        assert_eq!(mock.sent_count() - before, 1, "no referral round trip");
    }

    /// § 3.1.5.4.5: ReferralServers set with StorageServers clear means the
    /// target lives in another namespace, so it is re-resolved rather than
    /// connected.
    #[tokio::test]
    async fn an_interlink_is_resolved_again_rather_than_connected() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;

        mock.queue_response(build_tree_connect_refusal(
            NtStatus::BAD_NETWORK_NAME,
            false,
        ));
        mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
        // R set, S clear: an interlink pointing at a second namespace.
        mock.queue_response(build_root_referral(
            r"\test-server\projects",
            &[r"\test-server\elsewhere"],
            0x01,
        ));
        // The second namespace resolves normally.
        mock.queue_response(build_root_referral(
            r"\test-server\elsewhere",
            &[r"\test-server\real_share"],
            0x02,
        ));
        mock.queue_response(build_tree_connect_response(TreeId(9), ShareType::Disk));

        let tree = client.connect_share("projects").await.unwrap();
        assert_eq!(tree.share_name, "real_share");
        assert_eq!(
            tree.dfs_origin.unwrap().requested,
            r"\\test-server\projects",
            "the caller's name survives however many hops it took"
        );
    }

    /// A namespace that refers to itself would resolve forever. It stops at
    /// `MAX_DFS_HOPS` with an error naming the path, not a hang.
    #[tokio::test]
    async fn a_namespace_that_refers_to_itself_stops() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;

        mock.queue_response(build_tree_connect_refusal(
            NtStatus::BAD_NETWORK_NAME,
            false,
        ));
        mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
        // An interlink naming its own namespace. The cache answers every hop
        // after the first, so this is one frame and eight hops.
        mock.queue_response(build_root_referral(
            r"\test-server\projects",
            &[r"\test-server\projects"],
            0x01,
        ));

        let err = client.connect_share("projects").await.unwrap_err();
        match err {
            Error::DfsTooManyReferrals { namespace, hops } => {
                assert_eq!(namespace, r"\\test-server\projects");
                assert_eq!(hops, MAX_DFS_HOPS);
            }
            other => panic!("expected DfsTooManyReferrals, got {other:?}"),
        }
    }

    /// A `Tree` outlives the pool entry it was resolved on, and using it then
    /// has to be an error rather than a panic inside a library.
    ///
    /// `reconnect()` drops every extra connection while the consumer still
    /// holds the DFS-resolved `Tree` it had — which is exactly the state the
    /// reconnect docs tell them they are in. Routing that tree used to
    /// `.expect()` its way into a panic.
    #[tokio::test]
    async fn a_tree_whose_dfs_connection_is_gone_is_an_error_not_a_panic() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;

        let orphan = a_dfs_target_tree("dfs-target:445", "secret");
        assert!(matches!(
            client.connections.for_tree(&orphan),
            Err(Error::Disconnected)
        ));

        // The batch methods report per item, so they say it per item.
        let results = client.stat_files(&orphan, &["a.txt", "b.txt"]).await;
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|r| matches!(r, Err(Error::Disconnected))));
    }

    /// `recover_tree` revives the DFS target's own connection and
    /// re-establishes the tree on it. It used to refuse outright, so a
    /// namespace root's session dying was unrecoverable.
    #[tokio::test]
    async fn a_dfs_target_tree_recovers_on_its_own_connection() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;

        let target_mock = Arc::new(MockTransport::new());
        pool_extra_connection(&mut client, "dfs-target:445", &target_mock);

        // The revived connection answers the re-tree-connect with a new id.
        let reviver = Arc::new(MockReviver::answering_with(
            SessionId(0xBEEF),
            vec![build_tree_connect_response(TreeId(77), ShareType::Disk)],
        ));

        let entry = client
            .connections
            .extra_mut("dfs-target:445")
            .expect("just pooled");
        entry.conn.set_reviver(Some(
            Arc::clone(&reviver) as Arc<dyn connection::SessionReviver>
        ));
        entry.conn.mark_dead();

        let mut tree = a_dfs_target_tree("dfs-target:445", "secret");
        client
            .recover_tree(&mut tree)
            .await
            .expect("a DFS target tree must be recoverable");

        assert_eq!(
            tree.tree_id,
            TreeId(77),
            "tree re-established on the new session"
        );
        assert_eq!(tree.server, "dfs-target:445", "still routed to the target");
        assert!(!client
            .connections
            .extra("dfs-target:445")
            .unwrap()
            .conn
            .is_disconnected());
        // The primary was not touched.
        assert_eq!(client.session().session_id, SessionId(0x77));
    }

    /// A DFS target share that asks for encryption gets it.
    ///
    /// `ensure_tree` is the path every DFS target goes through, and it skipped
    /// the activation `connect_share` does — so a target share carrying
    /// `SMB2_SHAREFLAG_ENCRYPT_DATA` moved the user's files in the clear.
    /// Both now go through `activate_share_encryption`.
    #[tokio::test]
    async fn a_dfs_target_share_that_asks_for_encryption_gets_it() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;

        let target_mock = Arc::new(MockTransport::new());
        pool_extra_connection(&mut client, "dfs-target:445", &target_mock);

        target_mock.queue_response(build_encrypting_tree_connect_response(TreeId(5)));
        let tree = client
            .ensure_tree("dfs-target:445", "secret")
            .await
            .expect("tree connect to the DFS target failed");

        assert!(tree.encrypt_data);
        assert!(
            client
                .connections
                .extra("dfs-target:445")
                .unwrap()
                .conn
                .should_encrypt(),
            "a DFS target share flagged SMB2_SHAREFLAG_ENCRYPT_DATA must be encrypted"
        );
    }

    /// A target share that did not ask for encryption does not get it, so the
    /// fix above cannot have quietly become "encrypt everything".
    #[tokio::test]
    async fn a_dfs_target_share_that_did_not_ask_is_left_alone() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x77)).await;

        let target_mock = Arc::new(MockTransport::new());
        pool_extra_connection(&mut client, "dfs-target:445", &target_mock);

        target_mock.queue_response(build_tree_connect_response(TreeId(5), ShareType::Disk));
        client.ensure_tree("dfs-target:445", "plain").await.unwrap();

        assert!(!client
            .connections
            .extra("dfs-target:445")
            .unwrap()
            .conn
            .should_encrypt());
    }

    /// Without keys there is nothing to encrypt with, and silently pretending
    /// otherwise would be worse than leaving it off.
    #[tokio::test]
    async fn an_encrypting_share_without_session_keys_is_left_alone() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x78)).await;

        let keyless = Session {
            encryption_key: None,
            decryption_key: None,
            ..session_with_keys(SessionId(0x78))
        };
        let tree = Tree {
            pin: None,
            tree_id: TreeId(1),
            share_name: "secret".to_string(),
            server: "test-server:445".to_string(),
            is_dfs: false,
            encrypt_data: true,
            dfs_origin: None,
        };

        activate_share_encryption(client.connection_mut(), &keyless, &tree);
        assert!(!client.connection_mut().should_encrypt());
    }

    #[tokio::test]
    async fn smb_client_connect_via_mock_negotiates_and_authenticates() {
        let mock = Arc::new(MockTransport::new());
        let session_id = SessionId(0xABCD);

        let client = make_mock_client(&mock, session_id).await;

        assert_eq!(client.session().session_id, session_id);
        assert!(client.params().is_some());
        assert_eq!(client.params().unwrap().dialect, Dialect::Smb3_1_1);
    }

    #[tokio::test]
    async fn smb_client_stores_config() {
        let mock = Arc::new(MockTransport::new());
        let client = make_mock_client(&mock, SessionId(1)).await;

        assert_eq!(client.config().addr, "test-server:445");
        assert_eq!(client.config().username, "");
        assert_eq!(client.config().password, "");
        assert!(!client.config().auto_reconnect);
    }

    #[tokio::test]
    async fn smb_client_connect_share_returns_tree() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(1)).await;

        // Queue tree connect response.
        mock.queue_response(crate::client::test_helpers::build_tree_connect_response(
            TreeId(42),
            ShareType::Disk,
        ));

        let tree = client.connect_share("TestShare").await.unwrap();
        assert_eq!(tree.tree_id, TreeId(42));
        assert_eq!(tree.share_name, "TestShare");
    }

    /// A reviver that answers each dial with a fresh mock transport, already
    /// loaded with a negotiate + session-setup conversation. Exercises the real
    /// `negotiate` and `Session::setup` paths, unlike the scripted-server
    /// double in `fault_injection_tests`, which is deliberately about the
    /// revival machinery rather than the protocol.
    struct MockReviver {
        session_id: SessionId,
        dialed: std::sync::atomic::AtomicUsize,
        /// Queued behind the negotiate + session-setup conversation, for a
        /// test whose caller does more on the revived connection.
        after_setup: std::sync::Mutex<Vec<Vec<u8>>>,
    }

    impl MockReviver {
        fn new(session_id: SessionId) -> Self {
            Self {
                session_id,
                dialed: std::sync::atomic::AtomicUsize::new(0),
                after_setup: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// Also answer `responses`, in order, once the revived session is up.
        fn answering_with(session_id: SessionId, responses: Vec<Vec<u8>>) -> Self {
            Self {
                after_setup: std::sync::Mutex::new(responses),
                ..Self::new(session_id)
            }
        }
    }

    #[async_trait::async_trait]
    impl connection::SessionReviver for MockReviver {
        async fn dial(
            &self,
        ) -> Result<(
            Box<dyn crate::transport::TransportSend>,
            Box<dyn crate::transport::TransportReceive>,
        )> {
            self.dialed
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mock = Arc::new(MockTransport::new());
            mock.enable_auto_rewrite_msg_id();
            queue_negotiate_and_session(&mock, self.session_id);
            for response in self.after_setup.lock().unwrap().drain(..) {
                mock.queue_response(response);
            }
            Ok((Box::new(mock.clone()), Box::new(mock)))
        }

        async fn reauthenticate(&self, conn: &mut Connection) -> Result<()> {
            conn.negotiate().await?;
            Session::setup(conn, "", "", "").await?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn smb_client_reconnect_creates_new_session() {
        let mock = Arc::new(MockTransport::new());
        let original_session_id = SessionId(0x1111);
        let mut client = make_mock_client(&mock, original_session_id).await;
        assert_eq!(client.session().session_id, original_session_id);

        let reviver = Arc::new(MockReviver::new(SessionId(0x2222)));
        client
            .connections
            .primary()
            .set_reviver(Some(reviver.clone()));
        client.reconnect().await.unwrap();

        assert_eq!(
            client.session().session_id,
            SessionId(0x2222),
            "the client must adopt the session the revival established, not \
             keep signing with the dead one's keys"
        );
        assert_eq!(reviver.dialed.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    /// The reason reconnect happens in place: a `Connection` clone taken before
    /// the reconnect keeps working afterwards.
    ///
    /// Consumers hand these clones out everywhere — a `FileWriter` mid-upload,
    /// a `Watcher`, every task in a pipelined transfer. If a reconnect minted a
    /// fresh `Connection` instead, all of them would be permanently dead and
    /// the consumer would have to rebuild the world, which is reporting the
    /// blip rather than surviving it.
    #[tokio::test]
    async fn a_connection_clone_taken_before_a_reconnect_is_alive_after_it() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x1111)).await;
        let held = client.connections.primary().clone();
        assert_eq!(held.generation(), 0);

        client
            .connections
            .primary()
            .set_reviver(Some(Arc::new(MockReviver::new(SessionId(0x2222)))));
        client.reconnect().await.unwrap();

        assert!(
            !held.is_disconnected(),
            "the clone somebody was mid-transfer on must come back with the \
             connection"
        );
        assert_eq!(held.generation(), 1, "and know it is on a new session");
        assert_eq!(held.session_id(), SessionId(0x2222));
    }

    #[tokio::test]
    async fn smb_client_reconnect_renegotiates_params() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x1111)).await;
        let old_server_guid = client.params().unwrap().server_guid;

        client
            .connections
            .primary()
            .set_reviver(Some(Arc::new(MockReviver::new(SessionId(0x2222)))));
        client.reconnect().await.unwrap();

        // Freshly negotiated. The mock answers with the same numbers, but they
        // came from the new socket -- a rebooted server offering a smaller
        // MaxWriteSize would be reflected here, which a `OnceLock` could not do.
        assert!(client.params().is_some());
        assert_eq!(client.params().unwrap().server_guid, old_server_guid);
    }

    /// A reviver that brings up a whole working share: negotiate, session
    /// setup, tree connect, and one compound CREATE+READ+CLOSE ready to serve.
    struct RevivedShare {
        session_id: SessionId,
        tree_id: TreeId,
        contents: Vec<u8>,
        dialed: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl connection::SessionReviver for RevivedShare {
        async fn dial(
            &self,
        ) -> Result<(
            Box<dyn crate::transport::TransportSend>,
            Box<dyn crate::transport::TransportReceive>,
        )> {
            self.dialed
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mock = Arc::new(MockTransport::new());
            mock.enable_auto_rewrite_msg_id();
            queue_negotiate_and_session(&mock, self.session_id);
            mock.queue_response(crate::client::test_helpers::build_tree_connect_response(
                self.tree_id,
                ShareType::Disk,
            ));
            mock.queue_response(crate::client::test_helpers::build_compound_response_frame(
                &[
                    crate::client::test_helpers::build_create_response(
                        FileId {
                            persistent: 9,
                            volatile: 9,
                        },
                        self.contents.len() as u64,
                    ),
                    crate::client::test_helpers::build_read_response(self.contents.clone()),
                    crate::client::test_helpers::build_close_response(),
                ],
            ));
            Ok((Box::new(mock.clone()), Box::new(mock)))
        }

        async fn reauthenticate(&self, conn: &mut Connection) -> Result<()> {
            conn.negotiate().await?;
            Session::setup(conn, "", "", "").await?;
            Ok(())
        }
    }

    /// Build a client on a dead connection, armed to reconnect into
    /// `RevivedShare`.
    async fn client_on_a_dead_session(reviver: Arc<RevivedShare>) -> (SmbClient, Tree) {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x1111)).await;
        mock.queue_response(crate::client::test_helpers::build_tree_connect_response(
            TreeId(1),
            ShareType::Disk,
        ));
        let tree = client.connect_share("TestShare").await.unwrap();

        client.config.auto_reconnect = true;
        client.connections.primary().set_reviver(Some(reviver));
        client.connections.primary().mark_dead(); // the NAS went away
        (client, tree)
    }

    /// What `auto_reconnect` buys at the client level: a read that lands on a
    /// dead session comes back anyway, on a new one, with the tree re-connected
    /// underneath the caller's `&mut Tree`.
    #[tokio::test]
    async fn a_read_on_a_dead_session_reconnects_and_returns_the_data() {
        let reviver = Arc::new(RevivedShare {
            session_id: SessionId(0x2222),
            tree_id: TreeId(77),
            contents: b"survived the blip".to_vec(),
            dialed: std::sync::atomic::AtomicUsize::new(0),
        });
        let (mut client, mut tree) = client_on_a_dead_session(reviver.clone()).await;

        let data = tokio::time::timeout(
            Duration::from_secs(10),
            client.read_file_compound(&mut tree, "notes.txt"),
        )
        .await
        .expect("the read hung, which is the bug")
        .expect("auto_reconnect should have recovered this read");

        assert_eq!(data, b"survived the blip");
        assert_eq!(
            reviver.dialed.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "one death, one dial"
        );
        assert_eq!(
            tree.tree_id,
            TreeId(77),
            "the caller's tree must be re-established in place -- the old tree \
             id belongs to a session that no longer exists"
        );
        assert_eq!(client.session().session_id, SessionId(0x2222));
    }

    /// A tree from before a revival somebody else already completed: the
    /// connection is alive, so the read doesn't fail with `Disconnected`, it
    /// fails with `HandleInvalidated` before the stale tree id leaves. That
    /// still counts as a dead session for the replay, which reconnects the
    /// share and reads on the new tree.
    #[tokio::test]
    async fn a_stale_tree_on_an_already_revived_connection_heals_in_a_replaying_read() {
        let reviver = Arc::new(RevivedShare {
            session_id: SessionId(0x2222),
            tree_id: TreeId(77),
            contents: b"healed".to_vec(),
            dialed: std::sync::atomic::AtomicUsize::new(0),
        });
        let (mut client, mut tree) = client_on_a_dead_session(reviver.clone()).await;
        client
            .connections
            .primary()
            .reconnect_if_needed()
            .await
            .expect("another task revives the connection first");

        let data = tokio::time::timeout(
            Duration::from_secs(10),
            client.read_file_compound(&mut tree, "notes.txt"),
        )
        .await
        .expect("the read hung")
        .expect("a stale tree on a live connection should be recovered and replayed");

        assert_eq!(data, b"healed");
        assert_eq!(tree.tree_id, TreeId(77), "the caller's tree is the new one");
        assert_eq!(
            reviver.dialed.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the replay reconnects the share, not the connection"
        );
    }

    /// The data-safety boundary: a mutating operation is NEVER replayed across
    /// a reconnect.
    ///
    /// A DELETE that died in flight may already have taken effect on the
    /// server. Re-running it after a reconnect turns "it worked" into "not
    /// found" and, worse, could delete a file the user recreated in between.
    /// The library has no way to tell, so it surfaces the error and lets the
    /// caller decide.
    #[tokio::test]
    async fn a_mutating_operation_is_never_replayed_across_a_reconnect() {
        let reviver = Arc::new(RevivedShare {
            session_id: SessionId(0x2222),
            tree_id: TreeId(77),
            contents: Vec::new(),
            dialed: std::sync::atomic::AtomicUsize::new(0),
        });
        let (mut client, tree) = client_on_a_dead_session(reviver.clone()).await;

        for (what, outcome) in [
            ("delete", client.delete_file(&tree, "x.txt").await.err()),
            ("rename", client.rename(&tree, "a", "b").await.err()),
            ("mkdir", client.create_directory(&tree, "d").await.err()),
            (
                "write",
                client.write_file(&tree, "x.txt", b"data").await.err(),
            ),
        ] {
            assert!(
                outcome.is_some(),
                "{what} on a dead session must fail, not silently re-run"
            );
        }
        assert_eq!(
            reviver.dialed.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "nothing may reconnect-and-replay an operation that could already \
             have taken effect on the server"
        );
    }

    /// `auto_reconnect` is what arms the connection, and it is off by default.
    #[tokio::test]
    async fn auto_reconnect_off_leaves_the_connection_unarmed() {
        let mock = Arc::new(MockTransport::new());
        let client = make_mock_client(&mock, SessionId(1)).await;
        assert!(!client.config().auto_reconnect);
        assert!(
            !client.connections.primary().can_reconnect(),
            "nothing should dial on a consumer's behalf without being asked"
        );
    }

    #[tokio::test]
    async fn smb_client_auto_reconnect_flag_stored() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        queue_negotiate_and_session(mock.as_ref(), SessionId(1));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.negotiate().await.unwrap();
        let session = Session::setup(&mut conn, "", "", "").await.unwrap();

        let config = ClientConfig {
            addr: "test-server:445".to_string(),
            timeout: Duration::from_secs(5),
            username: String::new(),
            password: String::new(),
            domain: String::new(),
            auto_reconnect: true,
            compression: true,
            dfs_enabled: true,
            dfs_target_overrides: std::collections::HashMap::new(),
            connect_options: None,
            ..Default::default()
        };

        let client = SmbClient::from_parts(config, conn, session);
        assert!(client.config().auto_reconnect);
    }

    #[tokio::test]
    async fn smb_client_connection_mut_returns_connection() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(1)).await;

        // Verify we can access the connection.
        assert!(client.connection_mut().params().is_some());
    }

    #[tokio::test]
    async fn smb_client_list_shares_delegates_to_shares_module() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x5555)).await;

        // Queue the full share listing flow (same as shares module tests).
        // This verifies SmbClient.list_shares() delegates correctly.
        use crate::client::shares::tests::queue_share_listing_responses;
        queue_share_listing_responses(
            &mock,
            &[
                (
                    "Documents",
                    crate::rpc::srvsvc::STYPE_DISKTREE,
                    "Shared docs",
                ),
                (
                    "IPC$",
                    crate::rpc::srvsvc::STYPE_IPC | crate::rpc::srvsvc::STYPE_SPECIAL,
                    "Remote IPC",
                ),
            ],
        );

        let shares = client.list_shares().await.unwrap();

        // Only disk shares returned.
        assert_eq!(shares.len(), 1);
        assert_eq!(shares[0].name, "Documents");
    }

    /// The host half of an address, for the UNC path and the DFS cache key.
    ///
    /// Every IPv6 form is here because the old `split(':')` got every one of
    /// them wrong (`[::1]:445` → `[`, `fe80::1:445` → `fe80`) while looking
    /// perfectly healthy on IPv4, which is why it survived so long.
    #[test]
    fn a_host_is_read_off_an_address_without_eating_an_ipv6_literal() {
        let cases = [
            // (address, host)
            ("192.168.1.111:445", "192.168.1.111"),
            ("naspolya.local:445", "naspolya.local"),
            ("corp.example.com:445", "corp.example.com"),
            // Bracketed: the brackets say where the literal ends, and come off.
            ("[::1]:445", "::1"),
            ("[2001:db8::5]:445", "2001:db8::5"),
            // Bracket-less: the port is what follows the LAST colon, which is
            // how `ToSocketAddrs` reads it, so this agrees with what we dialled.
            ("fe80::1:445", "fe80::1"),
            ("2001:db8::5:445", "2001:db8::5"),
            // No port: nothing to strip.
            ("naspolya.local", "naspolya.local"),
            ("[::1]", "::1"),
        ];
        for (addr, expected) in cases {
            assert_eq!(super::host_of(addr), expected, "host of {addr:?}");
        }
    }

    /// The UNC path and the address share one derivation, so a DFS cache key
    /// and the tree-connect path can't name two different servers.
    #[test]
    fn the_unc_path_and_the_dialled_address_agree_on_the_server() {
        for addr in [
            "192.168.1.111:445",
            "[::1]:445",
            "fe80::1:445",
            "nas.local:445",
        ] {
            let host = super::host_of(addr);
            assert!(
                !host.contains(':') || !host.starts_with('['),
                "a bracket survived into the UNC path for {addr:?}: {host:?}"
            );
            assert!(!host.is_empty(), "empty host for {addr:?}");
            assert_ne!(host, "[", "the bracket-only bug is back for {addr:?}");
        }
    }

    /// A listener that accepts and then says nothing, ever: the server shape
    /// behind cmdr#406. Returns its address; the accepted sockets stay open
    /// for as long as the test runtime lives.
    async fn silent_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        addr
    }

    /// `ClientConfig::response_timeout` is the only way to bound the NEGOTIATE
    /// inside `SmbClient::connect`: no setter can reach a connection before
    /// `connect` returns it.
    #[tokio::test]
    async fn connect_to_a_silent_server_gives_up_within_the_configured_response_timeout() {
        let config = ClientConfig {
            addr: silent_server().await,
            response_timeout: Some(Duration::from_millis(200)),
            ..Default::default()
        };

        let result = tokio::time::timeout(Duration::from_secs(5), SmbClient::connect(config))
            .await
            .expect("connect must give up on its own, not wait out the 30 s default");

        assert!(
            matches!(
                result,
                Err(Error::Timeout) | Err(Error::ServerUnresponsive { .. })
            ),
            "expected a timeout, got {:?}",
            result.err()
        );
    }

    /// Whether a referral may send this client's credentials to another host.
    ///
    /// Each test points a referral at `evil-host`, mapped to a real listener,
    /// and counts the connections it accepts: none means no credentials went
    /// anywhere, by construction.
    mod dfs_consent {
        use super::*;
        use std::sync::atomic::AtomicBool;

        /// A listener that counts connections and hangs up on each.
        async fn counting_listener() -> (String, Arc<AtomicU64>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            let accepted = Arc::new(AtomicU64::new(0));
            let counter = Arc::clone(&accepted);
            tokio::spawn(async move {
                while let Ok((socket, _)) = listener.accept().await {
                    counter.fetch_add(1, Ordering::SeqCst);
                    drop(socket);
                }
            });
            (addr, accepted)
        }

        async fn client_with(
            mock: &Arc<MockTransport>,
            policy: DfsTargetPolicy,
        ) -> (SmbClient, Arc<AtomicU64>) {
            let mut client = make_mock_client(mock, SessionId(0x77)).await;
            let (addr, accepted) = counting_listener().await;
            client.config.dfs_target_policy = policy;
            client
                .config
                .dfs_target_overrides
                .insert("evil-host".to_string(), addr);
            (client, accepted)
        }

        fn queue_evil_link(mock: &MockTransport) {
            mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
            mock.queue_response(build_referral(
                3,
                0,
                r"\test-server\dfs\data",
                &[r"\evil-host\loot"],
                0,
            ));
        }

        fn assert_refused(err: &Error, path: &str) {
            match err {
                Error::DfsTargetNotAllowed {
                    path: asked,
                    server,
                    target,
                } => {
                    assert_eq!(asked, path);
                    assert_eq!(server, "test-server");
                    assert_eq!(target, "evil-host");
                }
                other => panic!("expected DfsTargetNotAllowed, got {other:?}"),
            }
            assert_eq!(err.kind(), crate::ErrorKind::DfsTargetNotAllowed);
            assert!(!err.is_retryable(), "only a change of policy helps");
        }

        #[tokio::test]
        async fn the_default_policy_is_refusal() {
            assert!(matches!(
                ClientConfig::default().dfs_target_policy,
                DfsTargetPolicy::SameServerOnly
            ));
        }

        /// The headline. A followed call and the resolver both refuse, typed,
        /// and the other host hears nothing, then or from the cached referral.
        #[tokio::test]
        async fn a_different_host_hears_nothing_without_consent() {
            let mock = Arc::new(MockTransport::new());
            let (mut client, accepted) = client_with(&mock, DfsTargetPolicy::SameServerOnly).await;
            let tree = a_dfs_root_tree();

            mock.queue_response(path_not_covered(3));
            queue_evil_link(&mock);
            let followed = client.delete_file(&tree, "data/a").await.unwrap_err();
            assert_refused(&followed, "data/a");

            let resolved = client
                .resolve_dfs_link(&tree, "data/b")
                .await
                .expect_err("cached referral, still refused");
            assert_refused(&resolved, "data/b");

            assert_eq!(accepted.load(Ordering::SeqCst), 0);
            mock.assert_fully_consumed();
        }

        #[tokio::test]
        async fn the_person_saying_no_is_the_same_as_the_default() {
            let mock = Arc::new(MockTransport::new());
            let (mut client, accepted) =
                client_with(&mock, DfsTargetPolicy::Ask(Arc::new(|_| false))).await;
            queue_evil_link(&mock);

            let err = client
                .resolve_dfs_link(&a_dfs_root_tree(), "data/a")
                .await
                .unwrap_err();

            assert_refused(&err, "data/a");
            assert_eq!(accepted.load(Ordering::SeqCst), 0);
        }

        /// What proves the counter counts: with consent the same call does dial.
        #[tokio::test]
        async fn consent_for_the_pair_lets_the_dial_through() {
            let mock = Arc::new(MockTransport::new());
            let (mut client, accepted) = client_with(
                &mock,
                DfsTargetPolicy::Ask(Arc::new(|sign_in| {
                    sign_in.server == "test-server" && sign_in.target == "evil-host"
                })),
            )
            .await;
            queue_evil_link(&mock);

            let err = client
                .resolve_dfs_link(&a_dfs_root_tree(), "data/a")
                .await
                .unwrap_err();

            assert_eq!(err.kind(), crate::ErrorKind::DfsTargetUnreachable);
            assert!(accepted.load(Ordering::SeqCst) >= 1);
        }

        /// A link on the share's own server never asks, whatever its case or
        /// trailing dot: it runs on the connection the client already has.
        #[tokio::test]
        async fn a_link_on_the_shares_own_server_goes_through() {
            let mock = Arc::new(MockTransport::new());
            let (mut client, _) =
                client_with(&mock, DfsTargetPolicy::Ask(Arc::new(|_| panic!("asked")))).await;
            queue_link_referral(&mock);

            let link = client
                .resolve_dfs_link(&a_dfs_root_tree(), "data/a")
                .await
                .expect("resolve")
                .expect("behind the link");

            assert_eq!(link.tree.tree_id, LINK_TARGET_TREE);
        }

        /// Forgetting takes effect on the next call: a pooled target isn't
        /// handed out, though nothing new would have been sent to it.
        #[tokio::test]
        async fn forgetting_consent_stops_handing_out_a_pooled_target() {
            let mock = Arc::new(MockTransport::new());
            let allowed = Arc::new(AtomicBool::new(true));
            let answer = Arc::clone(&allowed);
            let (mut client, accepted) = client_with(
                &mock,
                DfsTargetPolicy::Ask(Arc::new(move |_| answer.load(Ordering::SeqCst))),
            )
            .await;
            let addr = client.config.dfs_target_overrides["evil-host"].clone();
            let target_mock = Arc::new(MockTransport::new());
            pool_extra_connection(&mut client, &addr, &target_mock);
            client
                .connections
                .remember_link_tree(&a_dfs_target_tree(&addr, "loot"));
            let tree = a_dfs_root_tree();
            queue_evil_link(&mock);

            let link = client.resolve_dfs_link(&tree, "data/a").await.unwrap();
            assert!(link.is_some(), "allowed: the pooled target serves it");

            allowed.store(false, Ordering::SeqCst);
            let err = client.resolve_dfs_link(&tree, "data/a").await.unwrap_err();
            assert_refused(&err, "data/a");
            assert_eq!(accepted.load(Ordering::SeqCst), 0);
        }

        /// A namespace root takes the same gate, and the caller hears why rather
        /// than the refused tree connect it would otherwise get.
        #[tokio::test]
        async fn a_namespace_root_on_a_different_host_hears_nothing_without_consent() {
            let mock = Arc::new(MockTransport::new());
            let (mut client, accepted) = client_with(&mock, DfsTargetPolicy::SameServerOnly).await;
            mock.queue_response(build_tree_connect_refusal(
                NtStatus::BAD_NETWORK_NAME,
                false,
            ));
            mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
            mock.queue_response(build_root_referral(
                r"\test-server\projects",
                &[r"\evil-host\loot"],
                0x02,
            ));

            let err = client.connect_share("projects").await.unwrap_err();

            assert_refused(&err, r"\\test-server\projects");
            assert_eq!(accepted.load(Ordering::SeqCst), 0);
        }

        /// An Interlink sends the next referral to the host it names, which is a
        /// sign-in on that host before any target comes into it.
        #[tokio::test]
        async fn an_interlink_to_a_different_host_hears_nothing_without_consent() {
            let mock = Arc::new(MockTransport::new());
            let (mut client, accepted) = client_with(&mock, DfsTargetPolicy::SameServerOnly).await;
            mock.queue_response(build_tree_connect_refusal(
                NtStatus::BAD_NETWORK_NAME,
                false,
            ));
            mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
            mock.queue_response(build_root_referral(
                r"\test-server\projects",
                &[r"\evil-host\ns"],
                0x01,
            ));

            let err = client.connect_share("projects").await.unwrap_err();

            assert_refused(&err, r"\\test-server\projects");
            assert_eq!(accepted.load(Ordering::SeqCst), 0);
        }

        /// A refused target doesn't stop a permitted one; with no permitted one
        /// left the refusal is the answer, ahead of any unreachable target.
        #[tokio::test]
        async fn a_refused_target_falls_through_to_the_shares_own_server() {
            let mock = Arc::new(MockTransport::new());
            let (mut client, accepted) = client_with(&mock, DfsTargetPolicy::SameServerOnly).await;
            mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
            mock.queue_response(build_referral(
                3,
                0,
                r"\test-server\dfs\data",
                &[r"\evil-host\loot", r"\test-server\files"],
                0,
            ));
            mock.queue_response(build_tree_connect_response(
                LINK_TARGET_TREE,
                ShareType::Disk,
            ));

            let link = client
                .resolve_dfs_link(&a_dfs_root_tree(), "data/a")
                .await
                .expect("resolve")
                .expect("behind the link");

            assert_eq!(link.tree.tree_id, LINK_TARGET_TREE);
            assert_eq!(accepted.load(Ordering::SeqCst), 0);
        }

        #[tokio::test]
        async fn a_refusal_outranks_a_target_that_doesnt_answer() {
            let mock = Arc::new(MockTransport::new());
            let (mut client, _) = client_with(&mock, DfsTargetPolicy::SameServerOnly).await;
            mock.queue_response(build_tree_connect_response(TreeId(2), ShareType::Pipe));
            mock.queue_response(build_referral(
                3,
                0,
                r"\test-server\dfs\data",
                &[r"\test-server\files", r"\evil-host\loot"],
                0,
            ));
            mock.queue_response(build_tree_connect_refusal(NtStatus::ACCESS_DENIED, false));

            let err = client
                .resolve_dfs_link(&a_dfs_root_tree(), "data/a")
                .await
                .unwrap_err();

            assert_refused(&err, "data/a");
        }

        /// A target's reviver holds the password and dials on its own, from any
        /// connection clone. It asks too, so forgetting reaches a revival.
        #[tokio::test]
        async fn a_targets_reviver_asks_before_it_dials() {
            use crate::client::connection::SessionReviver;
            let (addr, accepted) = counting_listener().await;
            let config = ClientConfig {
                addr: "test-server:445".to_string(),
                dfs_target_policy: DfsTargetPolicy::Ask(Arc::new(|_| false)),
                ..Default::default()
            };
            let reviver = ClientReviver::for_target(&config, "evil-host", addr);

            let err = reviver.dial().await.err().expect("refused");

            assert_eq!(err.kind(), crate::ErrorKind::DfsTargetNotAllowed);
            assert_eq!(accepted.load(Ordering::SeqCst), 0);
        }
    }

    /// A DFS target connection is dialed long after `connect`, so it takes the
    /// deadline the primary has NOW, including anything the consumer set on it
    /// since, rather than falling back to the default.
    #[tokio::test]
    async fn a_dfs_target_connection_inherits_the_primary_response_timeout() {
        let mock = Arc::new(MockTransport::new());
        let mut client = make_mock_client(&mock, SessionId(0x1234)).await;
        client
            .connection()
            .set_response_timeout(Some(Duration::from_millis(200)));
        let target = silent_server().await;

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            client.ensure_connection(&target, &target, ""),
        )
        .await
        .expect("dialing a DFS target must give up within the primary's deadline");

        assert!(
            matches!(
                result,
                Err(Error::Timeout) | Err(Error::ServerUnresponsive { .. })
            ),
            "expected a timeout, got {result:?}"
        );
    }

    /// `{:?}` on a config is what a consumer logs first when a connect fails,
    /// so the password can't be in it. Everything else is.
    #[test]
    fn a_configs_debug_output_hides_the_password() {
        let config = ClientConfig {
            addr: "nas.local:445".to_string(),
            username: "david".to_string(),
            password: "hunter2-secret".to_string(),
            ..Default::default()
        };
        let printed = format!("{config:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(printed.contains("password: ***"), "{printed}");
        assert!(printed.contains("nas.local:445") && printed.contains("david"));
        assert!(
            printed.contains("response_timeout"),
            "every other field stays"
        );
    }
}

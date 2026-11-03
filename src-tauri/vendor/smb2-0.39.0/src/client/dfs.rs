//! DFS referral IOCTL helper and path resolver with referral cache.
//!
//! Sends `FSCTL_DFS_GET_REFERRALS` via IOCTL to resolve DFS paths. Connects
//! to IPC$ for the IOCTL exchange, similar to how `shares.rs` does for RPC.
//!
//! The [`DfsResolver`] caches referral responses with TTL and resolves UNC
//! paths by longest-prefix match, counted in whole path components (MS-DFSC
//! § 3.1.4.1), so `\\dom\ns` never swallows `\\dom\nsfoo\x`.
//!
//! Matching is case-insensitive, because DFS paths are, and **what comes back
//! keeps the caller's case**: a lowercased remaining path opens the wrong file,
//! or nothing, on a case-sensitive export.
//!
//! The cache mirrors § 3.1.1's ReferralCache: `RootOrLink`, `Interlink`,
//! `TargetFailback`, and a `TargetHint` that remembers which target last
//! worked. TTL is a single hard expiry; the spec's soft/hard split buys a
//! background refresh there is no scheduler for, and 600–1800 s TTLs make the
//! saving small.

// DFS resolver is used by SmbClient for reactive DFS path resolution.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use log::{debug, trace};

use crate::client::connection::Connection;
use crate::error::Result;
use crate::msg::dfs::{ReferralHeaderFlags, ReqGetDfsReferral, RespGetDfsReferral};
use crate::msg::ioctl::{
    IoctlRequest, IoctlResponse, FSCTL_DFS_GET_REFERRALS, SMB2_0_IOCTL_IS_FSCTL,
};
use crate::msg::tree_connect::{TreeConnectRequest, TreeConnectRequestFlags, TreeConnectResponse};
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::status::NtStatus;
use crate::types::{Command, FileId, TreeId};
use crate::Error;

/// Output buffer a referral asks for first (8 KiB).
///
/// Enough for any ordinary namespace. A root with many targets overflows it,
/// which is what [`DFS_RETRY_OUTPUT_RESPONSE`] is for.
const DFS_MAX_OUTPUT_RESPONSE: u32 = 8192;

/// What a referral asks for on the retry after `STATUS_BUFFER_OVERFLOW`
/// (64 KiB), per MS-DFSC § 3.1.5.4 and MS-SMB2 § 3.3.5.15.2.
const DFS_RETRY_OUTPUT_RESPONSE: u32 = 64 * 1024;

/// Send a DFS referral request and return the parsed response.
///
/// Reuses the connection's IPC$ tree, connecting one the first time
/// (MS-SMB2 § 3.2.4.20.3 lets a referral ride any existing tree connect and
/// only asks for a fresh IPC$ when none exists), then sends
/// `FSCTL_DFS_GET_REFERRALS` via IOCTL with `FileId::SENTINEL`.
///
/// The `path` should be a UNC-style path with a single leading backslash
/// (for example, `\server\share\dir`).
pub(crate) async fn get_dfs_referral(
    conn: &mut Connection,
    path: &str,
) -> Result<RespGetDfsReferral> {
    // Pinned before the cached IPC$ tree is read or connected, so its id goes
    // out only on the session that issued it.
    let conn = &mut conn.pinned_to_session();
    let tree_id = ensure_ipc_tree(conn).await?;

    match referral_ioctl(conn, tree_id, path).await {
        // The cached tree outlived the server's idea of it. Drop it, connect
        // a fresh one, and try once more; a stale id would otherwise turn
        // every later referral into this same error.
        Err(Error::Protocol {
            status: NtStatus::NETWORK_NAME_DELETED,
            ..
        }) => {
            debug!("dfs: IPC$ tree went away, reconnecting it");
            conn.set_cached_ipc_tree(None);
            let tree_id = ensure_ipc_tree(conn).await?;
            referral_ioctl(conn, tree_id, path).await
        }
        other => other,
    }
}

/// One referral IOCTL, growing the output buffer once if the server says the
/// first one was too small.
async fn referral_ioctl(
    conn: &mut Connection,
    tree_id: TreeId,
    path: &str,
) -> Result<RespGetDfsReferral> {
    match send_dfs_ioctl(conn, tree_id, path, DFS_MAX_OUTPUT_RESPONSE).await {
        Err(Error::Protocol {
            status: NtStatus::BUFFER_OVERFLOW,
            ..
        }) => {
            debug!(
                "dfs: referral overflowed {} bytes, retrying with {} path={:?}",
                DFS_MAX_OUTPUT_RESPONSE, DFS_RETRY_OUTPUT_RESPONSE, path
            );
            send_dfs_ioctl(conn, tree_id, path, DFS_RETRY_OUTPUT_RESPONSE).await
        }
        other => other,
    }
}

/// The connection's IPC$ tree, connecting one on first use.
async fn ensure_ipc_tree(conn: &mut Connection) -> Result<TreeId> {
    if let Some(tree_id) = conn.cached_ipc_tree() {
        return Ok(tree_id);
    }
    let tree_id = tree_connect_ipc(conn).await?;
    conn.set_cached_ipc_tree(Some(tree_id));
    Ok(tree_id)
}

/// Connect to the IPC$ share, returning the tree ID.
async fn tree_connect_ipc(conn: &mut Connection) -> Result<TreeId> {
    let server = conn.server_name().to_string();
    let unc_path = format!(r"\\{}\IPC$", server);

    let req = TreeConnectRequest {
        flags: TreeConnectRequestFlags::default(),
        path: unc_path,
    };

    let frame = conn.execute(Command::TreeConnect, &req, None).await?;

    if frame.header.command != Command::TreeConnect {
        return Err(Error::invalid_data(format!(
            "expected TreeConnect response, got {:?}",
            frame.header.command
        )));
    }

    if frame.header.status != NtStatus::SUCCESS {
        return Err(Error::Protocol {
            status: frame.header.status,
            command: Command::TreeConnect,
        });
    }

    let mut cursor = ReadCursor::new(&frame.body);
    let _resp = TreeConnectResponse::unpack(&mut cursor)?;

    let tree_id = frame
        .header
        .tree_id
        .ok_or_else(|| Error::invalid_data("TreeConnect response missing tree ID"))?;

    debug!("dfs: connected to IPC$, tree_id={}", tree_id);
    Ok(tree_id)
}

/// Build and send the FSCTL_DFS_GET_REFERRALS IOCTL, parse the response.
async fn send_dfs_ioctl(
    conn: &mut Connection,
    tree_id: TreeId,
    path: &str,
    max_output_response: u32,
) -> Result<RespGetDfsReferral> {
    // Build the referral request payload
    let referral_req = ReqGetDfsReferral {
        max_referral_level: 4,
        request_file_name: path.to_string(),
    };
    let mut req_cursor = WriteCursor::new();
    referral_req.pack(&mut req_cursor);
    let input_data = req_cursor.into_inner();

    trace!(
        "dfs: sending FSCTL_DFS_GET_REFERRALS ({} bytes input) path={:?}",
        input_data.len(),
        path
    );

    // Build the IOCTL request
    let ioctl_req = IoctlRequest {
        ctl_code: FSCTL_DFS_GET_REFERRALS,
        file_id: FileId::SENTINEL,
        max_input_response: 0,
        max_output_response,
        flags: SMB2_0_IOCTL_IS_FSCTL,
        input_data,
    };

    let frame = conn
        .execute(Command::Ioctl, &ioctl_req, Some(tree_id))
        .await?;

    if frame.header.status != NtStatus::SUCCESS {
        return Err(Error::Protocol {
            status: frame.header.status,
            command: Command::Ioctl,
        });
    }

    // Parse the IOCTL response envelope
    let mut cursor = ReadCursor::new(&frame.body);
    let ioctl_resp = IoctlResponse::unpack(&mut cursor)?;

    trace!(
        "dfs: received IOCTL response ({} bytes output)",
        ioctl_resp.output_data.len()
    );

    // Parse the DFS referral from the output buffer
    let mut ref_cursor = ReadCursor::new(&ioctl_resp.output_data);
    let referral_resp = RespGetDfsReferral::unpack(&mut ref_cursor)?;

    debug!(
        "dfs: parsed {} referral entries (path_consumed={})",
        referral_resp.entries.len(),
        referral_resp.path_consumed
    );

    Ok(referral_resp)
}

// ── DFS resolver types ───────────────────────────────────────────────

/// Whether a cache entry's targets are DFS root targets or link targets
/// (MS-DFSC § 3.1.1 `RootOrLink`, taken from a referral entry's `ServerType`).
///
/// ❌ **Stored, never gated on.** Samba answers a namespace-root referral with
/// `ServerType = 0`, which reads as `Link` here, where Windows answers 1. It
/// decides which spec step comes next, and nothing may make it a precondition
/// for accepting a referral at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootOrLink {
    /// The referral named DFS root targets (`ServerType = 1`).
    Root,
    /// The referral named DFS link targets (`ServerType = 0`).
    Link,
}

/// A resolved DFS path ready for connection.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedPath {
    /// Server hostname (or IP) to connect to.
    pub server: String,
    /// Port to connect on (default 445).
    pub port: u16,
    /// Share name to tree-connect.
    pub share: String,
    /// Remaining path within the share (may be empty), in the caller's own
    /// case.
    pub remaining_path: String,
    /// The referral points into another DFS namespace, so this target is a
    /// path to resolve again rather than somewhere to connect (MS-DFSC
    /// § 3.1.5.4.5).
    pub interlink: bool,
    /// Which cache entry this came from, and which of its targets, so
    /// [`DfsResolver::note_target_worked`] can record the `TargetHint`
    /// without a second lookup.
    cache_key: String,
    target_index: usize,
}

impl ResolvedPath {
    /// How many components of a path inside the share the referral covered:
    /// the link folder's depth, for example 1 for `\\server\share\data`.
    pub(crate) fn link_depth(&self) -> usize {
        path_components(&self.cache_key).len().saturating_sub(2)
    }
}

/// A single DFS target from a referral response.
#[derive(Debug, Clone)]
struct DfsTarget {
    /// Server hostname from the network_address field.
    server: String,
    /// Share name from the network_address field.
    share: String,
    /// Any remaining path suffix from the network_address.
    remaining_prefix: String,
}

/// A cached DFS referral entry with TTL (MS-DFSC § 3.1.1 ReferralCache).
#[derive(Debug, Clone)]
struct CachedReferral {
    /// The DFS path prefix this referral covers, one lowercased component per
    /// element. Components rather than a string because a hit has to align on
    /// a component boundary, and because lowercasing can change a string's
    /// byte length, so an index into the lowercased form does not map back to
    /// the caller's.
    prefix: Vec<String>,
    /// The same prefix as `\\server\share` text, for diagnostics.
    dfs_path_prefix: String,
    /// Root or link targets, per the referral's `ServerType`.
    root_or_link: RootOrLink,
    /// The response was an Interlink (§ 3.1.5.4.5).
    interlink: bool,
    /// The referral header's TargetFailback bit. V4 only; a server sets it to
    /// 0 for every other version.
    target_failback: bool,
    /// Available targets, in the order the server listed them.
    targets: Vec<DfsTarget>,
    /// Index into `targets` of the one that last worked (§ 3.1.1 TargetHint).
    /// Tried first, so a failover survives the next lookup instead of
    /// re-walking the dead target every time.
    target_hint: usize,
    /// When this entry expires.
    expires_at: Instant,
}

/// How many referral entries the cache holds before it starts evicting.
///
/// Keys come from server-supplied strings, so the cache has to be bounded by
/// something. A few hundred namespaces is far past what any real client
/// touches, and each entry is a few hundred bytes.
const MAX_CACHE_ENTRIES: usize = 256;

/// The fewest components a link's cache prefix has: server, share, and the
/// link folder. A namespace root's has two.
const LINK_MIN_COMPONENTS: usize = 3;

/// DFS referral cache and path resolver.
///
/// Maintains a cache of DFS referral responses keyed by path prefix.
/// Resolves UNC paths by longest-prefix matching against the cache,
/// falling back to an IOCTL referral request on cache miss.
pub(crate) struct DfsResolver {
    cache: HashMap<String, CachedReferral>,
    /// Counters surfaced through [`SmbClient::diagnostics`].
    cache_hits: AtomicU64,
    referrals_resolved: AtomicU64,
}

impl DfsResolver {
    /// Create a new empty resolver.
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            cache_hits: AtomicU64::new(0),
            referrals_resolved: AtomicU64::new(0),
        }
    }

    /// `(cache_hits, referrals_resolved)` for diagnostics.
    pub(crate) fn counters(&self) -> (u64, u64) {
        (
            self.cache_hits.load(Ordering::Relaxed),
            self.referrals_resolved.load(Ordering::Relaxed),
        )
    }

    /// Iterate the cache entries (including expired ones — eviction is
    /// lazy). Used by [`SmbClient::diagnostics`].
    pub(crate) fn cache_entries(&self) -> Vec<crate::client::diagnostics::DfsCacheEntry> {
        let now = Instant::now();
        self.cache
            .values()
            .map(|e| crate::client::diagnostics::DfsCacheEntry {
                path_prefix: e.dfs_path_prefix.clone(),
                target_count: e.targets.len(),
                target_hint: e.target_hint,
                root_targets: e.root_or_link == RootOrLink::Root,
                interlink: e.interlink,
                target_failback: e.target_failback,
                expires_in: if e.expires_at > now {
                    Some(e.expires_at - now)
                } else {
                    None
                },
            })
            .collect()
    }

    /// Resolve a UNC path by checking the cache first, then querying the server.
    ///
    /// `unc_path` should be like `\\server\share\path\to\file`.
    /// `conn` is the connection to the server that returned `STATUS_PATH_NOT_COVERED`.
    pub async fn resolve(
        &mut self,
        conn: &mut Connection,
        unc_path: &str,
    ) -> Result<Vec<ResolvedPath>> {
        // 1. Check cache (longest prefix match)
        if let Some(resolved) = self.resolve_from_cache(unc_path) {
            self.cache_hits.fetch_add(1, Ordering::Relaxed);
            trace!("dfs: cache hit path={unc_path:?}");
            return Ok(resolved);
        }

        // 2. Send referral request.
        // Convert \\server\share\path to \server\share\path (single leading
        // backslash for the IOCTL).
        let referral_path = if unc_path.starts_with("\\\\") {
            &unc_path[1..] // strip one leading backslash
        } else {
            unc_path
        };

        debug!("dfs: cache miss, sending a referral request path={referral_path:?}");
        let resp = get_dfs_referral(conn, referral_path).await?;
        self.referrals_resolved.fetch_add(1, Ordering::Relaxed);

        // 3. Cache the result
        self.cache_referral(referral_path, &resp);

        // 4. Resolve from the freshly cached entry
        self.resolve_from_cache(unc_path).ok_or_else(|| {
            Error::invalid_data("DFS referral response did not match the requested path")
        })
    }

    /// Try to resolve a path from the cache. Returns `None` on cache miss or
    /// expiry. Returns a `Vec` of [`ResolvedPath`]s (multiple targets for
    /// failover), with the `TargetHint` first.
    ///
    /// Matching is case-insensitive, because DFS paths are, and **the
    /// remaining path comes back in the caller's own case**: a case-sensitive
    /// export would open the wrong file, or nothing at all, from a lowercased
    /// one. It is also component-aligned, so `\\dom\ns` does not match
    /// `\\dom\nsfoo\x` (MS-DFSC § 3.1.4.1: "Whole path components are used in
    /// the match").
    pub(crate) fn resolve_from_cache(&self, unc_path: &str) -> Option<Vec<ResolvedPath>> {
        self.lookup(unc_path, 1)
    }

    /// The DFS link covering `unc_path` (`\\server\share\link\...`), from the
    /// cache alone: MS-DFSC § 3.1.4.1's check before a request goes out, so a
    /// path known to sit behind a link goes straight to its target instead
    /// of being refused first. Counts as a cache hit.
    ///
    /// Only a link's entry answers: its prefix names a folder inside a share,
    /// so at least three components. A namespace root's `\\dom\ns` covers
    /// every path under it too, and taking it would send the call back to
    /// the share it was refused on. An expired entry is a miss, so the call
    /// falls back to the refused CREATE and a fresh referral.
    pub(crate) fn link_from_cache(&self, unc_path: &str) -> Option<Vec<ResolvedPath>> {
        let resolved = self.lookup(unc_path, LINK_MIN_COMPONENTS)?;
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
        trace!("dfs: link cache hit path={unc_path:?}");
        Some(resolved)
    }

    /// The DFS link covering `unc_path`, from the cache or else a referral
    /// request over `conn`.
    ///
    /// `Ok(None)` when the server names no link there: it refused the
    /// referral (Samba 4.20 answers a path outside every link with
    /// `STATUS_OBJECT_PATH_NOT_FOUND`, MS-DFSC § 3.2.5.5 says
    /// `STATUS_NOT_FOUND`; verified against the `smb-dfs-root` fixture,
    /// 2026-10-30), or its answer covers no link. `Err` is the exchange
    /// itself failing.
    pub(crate) async fn resolve_link(
        &mut self,
        conn: &mut Connection,
        unc_path: &str,
    ) -> Result<Option<Vec<ResolvedPath>>> {
        if let Some(resolved) = self.link_from_cache(unc_path) {
            return Ok(Some(resolved));
        }
        let referral_path = unc_path.strip_prefix('\\').unwrap_or(unc_path);
        debug!("dfs: cache miss, sending a referral request path={referral_path:?}");
        let resp = match get_dfs_referral(conn, referral_path).await {
            Ok(resp) => resp,
            Err(Error::Protocol {
                status,
                command: Command::Ioctl,
            }) => {
                trace!("dfs: no link here ({status:?}) path={referral_path:?}");
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        self.referrals_resolved.fetch_add(1, Ordering::Relaxed);
        self.cache_referral(referral_path, &resp);
        Ok(self.lookup(unc_path, LINK_MIN_COMPONENTS))
    }

    /// Longest-prefix match over entries of at least `min_components`.
    fn lookup(&self, unc_path: &str, min_components: usize) -> Option<Vec<ResolvedPath>> {
        let components = path_components(unc_path);
        let now = Instant::now();

        // Longest prefix match, counted in components rather than bytes.
        let mut best: Option<&CachedReferral> = None;
        for entry in self.cache.values() {
            if entry.expires_at > now
                && entry.prefix.len() >= min_components
                && prefix_matches(&entry.prefix, &components)
                && best.is_none_or(|b| entry.prefix.len() > b.prefix.len())
            {
                best = Some(entry);
            }
        }

        let entry = best?;
        let remaining = components[entry.prefix.len()..].join("\\");

        let resolved: Vec<ResolvedPath> = entry
            .target_order()
            .map(|(target_index, target)| {
                let full_remaining = if target.remaining_prefix.is_empty() {
                    remaining.clone()
                } else if remaining.is_empty() {
                    target.remaining_prefix.clone()
                } else {
                    format!("{}\\{}", target.remaining_prefix, remaining)
                };

                ResolvedPath {
                    server: target.server.clone(),
                    port: 445,
                    share: target.share.clone(),
                    remaining_path: full_remaining,
                    interlink: entry.interlink,
                    cache_key: entry.dfs_path_prefix.clone(),
                    target_index,
                }
            })
            .collect();

        Some(resolved)
    }

    /// Remember that a target worked, so the next lookup tries it first
    /// (MS-DFSC § 3.1.1 TargetHint, § 3.1.5.2).
    ///
    /// Without this a failover is forgotten as soon as it succeeds, and every
    /// later lookup walks the dead target again.
    pub(crate) fn note_target_worked(&mut self, resolved: &ResolvedPath) {
        if let Some(entry) = self.cache.get_mut(&resolved.cache_key) {
            if resolved.target_index < entry.targets.len()
                && entry.target_hint != resolved.target_index
            {
                trace!(
                    "dfs: target hint moves to {} prefix={:?}",
                    resolved.target_index,
                    entry.dfs_path_prefix
                );
                entry.target_hint = resolved.target_index;
            }
        }
    }

    /// Store a referral response in the cache.
    ///
    /// `request_path` is the path the referral was asked for, which is the
    /// only way to key a V1 response: V1 entries have no `DFSPath` field, so
    /// the prefix has to come from `PathConsumed` against what we sent.
    fn cache_referral(&mut self, request_path: &str, resp: &RespGetDfsReferral) {
        let Some(first) = resp.entries.first() else {
            return;
        };

        let prefix_text = match first.dfs_path() {
            Some(dfs_path) => dfs_path.to_string(),
            None => match prefix_from_path_consumed(request_path, resp.path_consumed) {
                Some(prefix) => prefix.to_string(),
                None => {
                    debug!(
                        "dfs: referral carries no DFS path and path_consumed={} does not name \
                         a prefix of the path asked for; not caching path={:?}",
                        resp.path_consumed, request_path
                    );
                    return;
                }
            },
        };

        let prefix = path_components(&prefix_text)
            .iter()
            .map(|c| c.to_lowercase())
            .collect::<Vec<_>>();
        if prefix.is_empty() {
            return;
        }
        let dfs_path_prefix = format!("\\\\{}", prefix.join("\\"));

        let targets: Vec<DfsTarget> = resp
            .entries
            .iter()
            .filter_map(|e| parse_unc_target(e.target_address()?))
            .collect();

        if targets.is_empty() {
            return;
        }

        // ❌ `ServerType` decides what happens next, never whether we accept
        // the referral: Samba answers a root referral with 0.
        let root_or_link = match first.server_type() {
            Some(0) | None => RootOrLink::Link,
            Some(_) => RootOrLink::Root,
        };

        // A V1 entry has no TimeToLive at all, so give it the shortest useful
        // life rather than pretending the server said something.
        let ttl = first.ttl().max(1);

        debug!(
            "dfs: caching prefix={:?} targets={} ttl={}s, {:?}{}",
            dfs_path_prefix,
            targets.len(),
            ttl,
            root_or_link,
            if resp.header_flags.is_interlink() {
                ", interlink"
            } else {
                ""
            }
        );

        self.evict_for_insert(&dfs_path_prefix);
        self.cache.insert(
            dfs_path_prefix.clone(),
            CachedReferral {
                prefix,
                dfs_path_prefix,
                root_or_link,
                interlink: resp.header_flags.is_interlink(),
                target_failback: resp
                    .header_flags
                    .contains(ReferralHeaderFlags::TARGET_FAILBACK),
                targets,
                target_hint: 0,
                expires_at: Instant::now() + Duration::from_secs(ttl as u64),
            },
        );
    }

    /// Make room for one more entry: expired entries first, then whatever
    /// expires soonest.
    fn evict_for_insert(&mut self, incoming_key: &str) {
        if self.cache.len() < MAX_CACHE_ENTRIES || self.cache.contains_key(incoming_key) {
            return;
        }

        let now = Instant::now();
        self.cache.retain(|_, e| e.expires_at > now);
        while self.cache.len() >= MAX_CACHE_ENTRIES {
            let Some(soonest) = self
                .cache
                .iter()
                .min_by_key(|(_, e)| e.expires_at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            debug!("dfs: cache full, evicting prefix={soonest:?}");
            self.cache.remove(&soonest);
        }
    }
}

impl CachedReferral {
    /// Targets in the order to try them: the `TargetHint` first, then the
    /// server's own order.
    fn target_order(&self) -> impl Iterator<Item = (usize, &DfsTarget)> {
        let hint = self.target_hint.min(self.targets.len().saturating_sub(1));
        std::iter::once((hint, &self.targets[hint])).chain(
            self.targets
                .iter()
                .enumerate()
                .filter(move |(i, _)| *i != hint),
        )
    }
}

/// Split a UNC path into its components, treating both separators alike and
/// dropping empty runs. `\\dom\ns\dir\file` and `/dom/ns/dir/file` both give
/// `["dom", "ns", "dir", "file"]`.
fn path_components(path: &str) -> Vec<&str> {
    path.split(['\\', '/']).filter(|c| !c.is_empty()).collect()
}

/// Whether `prefix` (lowercased components) is a whole-component prefix of
/// `components`.
fn prefix_matches(prefix: &[String], components: &[&str]) -> bool {
    !prefix.is_empty()
        && prefix.len() <= components.len()
        && prefix
            .iter()
            .zip(components)
            .all(|(p, c)| p.as_str() == c.to_lowercase())
}

/// The prefix of `request_path` that `path_consumed` covers.
///
/// `PathConsumed` counts **bytes of UTF-16LE**, not characters (MS-DFSC
/// § 2.2.4), so the prefix is the first `path_consumed / 2` code units taken
/// with `encode_utf16` — never `chars()`, and never a byte slice. An odd
/// count, a zero, one longer than the path, or one that would split a
/// surrogate pair all mean the server and the client disagree about the path,
/// which is not something to guess at.
fn prefix_from_path_consumed(request_path: &str, path_consumed: u16) -> Option<&str> {
    if path_consumed == 0 || !path_consumed.is_multiple_of(2) {
        return None;
    }
    let want = usize::from(path_consumed / 2);

    let mut units = 0usize;
    for (byte_idx, ch) in request_path.char_indices() {
        if units == want {
            return Some(&request_path[..byte_idx]);
        }
        units += ch.len_utf16();
        if units > want {
            return None;
        }
    }
    (units == want).then_some(request_path)
}

/// Parse a UNC network_address into server, share, and remaining path.
///
/// Input: `\\server\share` or `\\server\share\path`.
/// Returns `None` if the format is invalid.
fn parse_unc_target(network_address: &str) -> Option<DfsTarget> {
    let path = network_address.trim_start_matches('\\');
    let mut parts = path.splitn(3, '\\');
    let server = parts.next()?.to_string();
    let share = parts.next()?.to_string();
    let remaining_prefix = parts.next().unwrap_or("").to_string();

    if server.is_empty() || share.is_empty() {
        return None;
    }

    Some(DfsTarget {
        server,
        share,
        remaining_prefix,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::client::connection::pack_message;
    use crate::client::test_helpers::{build_tree_connect_response, setup_connection};
    use crate::msg::dfs::{DfsReferralEntry, ReferralEntryFlags};
    use crate::msg::header::{ErrorResponse, Header};
    use crate::msg::ioctl::IoctlResponse as IoctlResp;
    use crate::msg::tree_connect::ShareType;
    use crate::transport::MockTransport;
    use crate::types::TreeId;
    use std::sync::Arc;

    /// Build an IOCTL response containing the given output data.
    fn build_ioctl_response(output_data: Vec<u8>) -> Vec<u8> {
        let mut h = Header::new_request(Command::Ioctl);
        h.flags.set_response();
        h.credits = 32;

        let body = IoctlResp {
            ctl_code: FSCTL_DFS_GET_REFERRALS,
            file_id: FileId::SENTINEL,
            flags: SMB2_0_IOCTL_IS_FSCTL,
            output_data,
        };

        pack_message(&h, &body)
    }

    /// Build an IOCTL error response with the given status.
    fn build_ioctl_error_response(status: NtStatus) -> Vec<u8> {
        let mut h = Header::new_request(Command::Ioctl);
        h.flags.set_response();
        h.credits = 32;
        h.status = status;

        let body = ErrorResponse {
            error_context_count: 0,
            error_data: vec![],
        };

        pack_message(&h, &body)
    }

    /// Pack a Samba-shaped V3 referral: `ServerType = 0`, whatever header
    /// flags the caller gives.
    pub(crate) fn pack_dfs_referral_response(
        path_consumed: u16,
        header_flags: u32,
        entries: &[(&str, &str, &str, u32)], // (dfs_path, alt_path, net_addr, ttl)
    ) -> Vec<u8> {
        pack_referral(3, 0, path_consumed, header_flags, entries)
    }

    /// Pack a referral at a chosen version and `ServerType`.
    ///
    /// The two shapes that matter are Samba's (V3, `ServerType = 0`, header
    /// flags `0x02`) and Windows' (V4, `ServerType = 1`, `0x03`, and
    /// TargetSetBoundary on the first entry, which MS-DFSC § 2.2.5.4 requires
    /// of a V4 response). The V4 entry layout is identical to V3's.
    pub(crate) fn pack_referral(
        version: u16,
        server_type: u16,
        path_consumed: u16,
        header_flags: u32,
        entries: &[(&str, &str, &str, u32)], // (dfs_path, alt_path, net_addr, ttl)
    ) -> Vec<u8> {
        // We build the referral response manually.
        // Entry fixed size: 4 (version+size) + 2+2+4 (server_type+flags+ttl)
        //   + 2+2+2 (offsets) + 16 (guid) = 34 bytes
        let entry_fixed_size: u16 = 34;
        let num_entries = entries.len() as u16;
        let total_fixed = entry_fixed_size * num_entries;

        // Pre-compute all string bytes
        let entry_strings: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> = entries
            .iter()
            .map(|(dfs, alt, net, _)| {
                (
                    encode_null_utf16(dfs),
                    encode_null_utf16(alt),
                    encode_null_utf16(net),
                )
            })
            .collect();

        // Compute cumulative string offsets relative to each entry's start.
        // All strings come after all fixed entries. The offset for entry i
        // is relative to entry i's start position.
        let mut buf = Vec::new();

        // Response header (8 bytes)
        buf.extend_from_slice(&path_consumed.to_le_bytes());
        buf.extend_from_slice(&num_entries.to_le_bytes());
        buf.extend_from_slice(&header_flags.to_le_bytes());

        // Calculate where strings start (after all fixed entries, but
        // offsets are measured from the start of the entry data, not from
        // the response header -- since RespGetDfsReferral::unpack reads
        // the header first and then works with the remaining bytes).
        //
        // Actually, offsets in V3 entries are relative to the entry start
        // within the entry data buffer.

        // Accumulate string buffer contents and compute per-entry offsets.
        let mut string_buf = Vec::new();
        let mut per_entry_offsets = Vec::new();

        for (i, (dfs_bytes, alt_bytes, net_bytes)) in entry_strings.iter().enumerate() {
            let entry_start = i as u16 * entry_fixed_size;
            let strings_base = total_fixed + string_buf.len() as u16;

            let dfs_offset = strings_base - entry_start;
            let alt_offset = dfs_offset + dfs_bytes.len() as u16;
            let net_offset = alt_offset + alt_bytes.len() as u16;

            per_entry_offsets.push((dfs_offset, alt_offset, net_offset));

            string_buf.extend_from_slice(dfs_bytes);
            string_buf.extend_from_slice(alt_bytes);
            string_buf.extend_from_slice(net_bytes);
        }

        // Write fixed entries
        for (i, (_, _, _, ttl)) in entries.iter().enumerate() {
            let (dfs_off, alt_off, net_off) = per_entry_offsets[i];

            buf.extend_from_slice(&version.to_le_bytes());
            buf.extend_from_slice(&entry_fixed_size.to_le_bytes()); // size
            buf.extend_from_slice(&server_type.to_le_bytes());
            // § 2.2.5.4: the first entry of a V4 response MUST open a target set.
            let entry_flags = if version == 4 && i == 0 {
                ReferralEntryFlags::TARGET_SET_BOUNDARY
            } else {
                0
            };
            buf.extend_from_slice(&entry_flags.to_le_bytes());
            buf.extend_from_slice(&ttl.to_le_bytes()); // ttl
            buf.extend_from_slice(&dfs_off.to_le_bytes());
            buf.extend_from_slice(&alt_off.to_le_bytes());
            buf.extend_from_slice(&net_off.to_le_bytes());
            buf.extend_from_slice(&[0u8; 16]); // service_site_guid
        }

        // Write string buffer
        buf.extend_from_slice(&string_buf);

        buf
    }

    /// Encode a string as null-terminated UTF-16LE bytes.
    pub(crate) fn encode_null_utf16(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for cu in s.encode_utf16() {
            out.extend_from_slice(&cu.to_le_bytes());
        }
        out.extend_from_slice(&[0x00, 0x00]);
        out
    }

    #[tokio::test]
    async fn dfs_referral_ioctl_flow() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        let tree_id = TreeId(99);

        // Build the DFS referral payload
        let referral_bytes = pack_dfs_referral_response(
            48,   // path_consumed
            0x02, // header_flags (StorageServers)
            &[
                (
                    r"\domain\dfs\docs",
                    r"\domain\dfs\docs",
                    r"\server1\share",
                    600,
                ),
                (
                    r"\domain\dfs\docs",
                    r"\domain\dfs\docs",
                    r"\server2\share",
                    300,
                ),
            ],
        );

        // Queue responses: TreeConnect, IOCTL
        mock.queue_response(build_tree_connect_response(tree_id, ShareType::Pipe));
        mock.queue_response(build_ioctl_response(referral_bytes));

        let resp = get_dfs_referral(&mut conn, r"\domain\dfs\docs")
            .await
            .unwrap();

        assert_eq!(resp.path_consumed, 48);
        assert_eq!(resp.header_flags.bits(), 0x02);
        assert_eq!(resp.entries.len(), 2);

        assert_eq!(resp.entries[0].version(), 3);
        assert_eq!(resp.entries[0].dfs_path(), Some(r"\domain\dfs\docs"));
        assert_eq!(resp.entries[0].target_address(), Some(r"\server1\share"));
        assert_eq!(resp.entries[0].ttl(), 600);

        assert_eq!(resp.entries[1].target_address(), Some(r"\server2\share"));
        assert_eq!(resp.entries[1].ttl(), 300);

        // TreeConnect + IOCTL. The IPC$ tree is kept, not torn down.
        assert_eq!(mock.sent_count(), 2);
        assert_eq!(conn.cached_ipc_tree(), Some(tree_id));
    }

    /// MS-SMB2 § 3.2.4.20.3 lets a referral ride an existing tree connect, so
    /// only the first one pays for IPC$. Three frames per referral become one.
    #[tokio::test]
    async fn dfs_referral_reuses_the_ipc_tree() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        let referral = |target: &str| {
            pack_dfs_referral_response(16, 0x02, &[(r"\domain\dfs", r"\domain\dfs", target, 600)])
        };

        mock.queue_response(build_tree_connect_response(TreeId(99), ShareType::Pipe));
        mock.queue_response(build_ioctl_response(referral(r"\server1\share")));
        mock.queue_response(build_ioctl_response(referral(r"\server2\share")));

        get_dfs_referral(&mut conn, r"\domain\dfs").await.unwrap();
        let second = get_dfs_referral(&mut conn, r"\domain\dfs").await.unwrap();

        assert_eq!(second.entries[0].target_address(), Some(r"\server2\share"));
        assert_eq!(mock.sent_count(), 3, "second referral should be one frame");
    }

    /// § 3.1.5.4: a referral that overflows the buffer is retried once with a
    /// bigger one. A namespace with many root targets outgrows 8 KiB, and the
    /// old code turned that into a hard error.
    #[tokio::test]
    async fn dfs_referral_retries_on_buffer_overflow() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        mock.queue_response(build_tree_connect_response(TreeId(99), ShareType::Pipe));
        mock.queue_response(build_ioctl_error_response(NtStatus::BUFFER_OVERFLOW));
        mock.queue_response(build_ioctl_response(pack_dfs_referral_response(
            16,
            0x02,
            &[(r"\domain\dfs", r"\domain\dfs", r"\server\share", 600)],
        )));

        let resp = get_dfs_referral(&mut conn, r"\domain\dfs").await.unwrap();
        assert_eq!(resp.entries[0].target_address(), Some(r"\server\share"));
        assert_eq!(mock.sent_count(), 3);
    }

    /// A cached tree id the session no longer has would otherwise poison
    /// every later referral with the same error.
    #[tokio::test]
    async fn dfs_referral_reconnects_a_deleted_ipc_tree() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        conn.set_cached_ipc_tree(Some(TreeId(7)));

        mock.queue_response(build_ioctl_error_response(NtStatus::NETWORK_NAME_DELETED));
        mock.queue_response(build_tree_connect_response(TreeId(42), ShareType::Pipe));
        mock.queue_response(build_ioctl_response(pack_dfs_referral_response(
            16,
            0x02,
            &[(r"\domain\dfs", r"\domain\dfs", r"\server\share", 600)],
        )));

        let resp = get_dfs_referral(&mut conn, r"\domain\dfs").await.unwrap();
        assert_eq!(resp.entries[0].target_address(), Some(r"\server\share"));
        assert_eq!(conn.cached_ipc_tree(), Some(TreeId(42)));
    }

    #[tokio::test]
    async fn dfs_referral_ioctl_error() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        let tree_id = TreeId(99);

        // Queue responses: TreeConnect, IOCTL error
        mock.queue_response(build_tree_connect_response(tree_id, ShareType::Pipe));
        mock.queue_response(build_ioctl_error_response(NtStatus::NOT_FOUND));

        let result = get_dfs_referral(&mut conn, r"\nonexistent\path").await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        match &err {
            Error::Protocol { status, command } => {
                assert_eq!(*status, NtStatus::NOT_FOUND);
                assert_eq!(*command, Command::Ioctl);
            }
            other => panic!("expected Protocol error, got: {other:?}"),
        }

        assert_eq!(mock.sent_count(), 2);
    }

    // ── parse_unc_target tests ───────────────────────────────────────

    #[test]
    fn parse_unc_target_basic() {
        let t = parse_unc_target(r"\\server\share").unwrap();
        assert_eq!(t.server, "server");
        assert_eq!(t.share, "share");
        assert_eq!(t.remaining_prefix, "");
    }

    #[test]
    fn parse_unc_target_with_path() {
        let t = parse_unc_target(r"\\server\share\path\to").unwrap();
        assert_eq!(t.server, "server");
        assert_eq!(t.share, "share");
        assert_eq!(t.remaining_prefix, r"path\to");
    }

    #[test]
    fn parse_unc_target_invalid() {
        assert!(parse_unc_target(r"\\").is_none());
        assert!(parse_unc_target("").is_none());
        assert!(parse_unc_target(r"\\server").is_none());
        // Single backslash + server but no share
        assert!(parse_unc_target(r"\server").is_none());
    }

    #[test]
    fn parse_unc_target_single_backslash_prefix() {
        // Network addresses with single backslash prefix should also work.
        let t = parse_unc_target(r"\server\share").unwrap();
        assert_eq!(t.server, "server");
        assert_eq!(t.share, "share");
        assert_eq!(t.remaining_prefix, "");
    }

    #[test]
    fn parse_unc_target_triple_backslash() {
        // Extra leading backslashes are stripped.
        let t = parse_unc_target(r"\\\server\share\path").unwrap();
        assert_eq!(t.server, "server");
        assert_eq!(t.share, "share");
        assert_eq!(t.remaining_prefix, "path");
    }

    #[test]
    fn parse_unc_target_ip_address() {
        // IP addresses as server names.
        let t = parse_unc_target(r"\\192.168.1.100\data").unwrap();
        assert_eq!(t.server, "192.168.1.100");
        assert_eq!(t.share, "data");
        assert_eq!(t.remaining_prefix, "");
    }

    #[test]
    fn parse_unc_target_deep_path() {
        // The remaining prefix captures everything after server\share.
        let t = parse_unc_target(r"\\server\share\a\b\c\d").unwrap();
        assert_eq!(t.server, "server");
        assert_eq!(t.share, "share");
        assert_eq!(t.remaining_prefix, r"a\b\c\d");
    }

    #[test]
    fn parse_unc_target_empty_components() {
        // Empty server or share should return None.
        assert!(parse_unc_target(r"\\\\share").is_none()); // empty server
        assert!(parse_unc_target(r"\\\").is_none()); // server is empty after strip
    }

    // ── DfsResolver tests ────────────────────────────────────────────

    /// Helper: build a RespGetDfsReferral for cache tests.
    fn make_referral(
        dfs_path: &str,
        entries: &[(&str, u32)], // (network_address, ttl)
    ) -> RespGetDfsReferral {
        use crate::msg::dfs::DfsReferralEntry;

        let referral_entries: Vec<DfsReferralEntry> = entries
            .iter()
            .map(|(net_addr, ttl)| DfsReferralEntry::Target {
                version: 3,
                server_type: 0,
                flags: crate::msg::dfs::ReferralEntryFlags::default(),
                ttl: *ttl,
                dfs_path: dfs_path.to_string(),
                dfs_alternate_path: dfs_path.to_string(),
                network_address: net_addr.to_string(),
            })
            .collect();

        RespGetDfsReferral {
            path_consumed: 0,
            header_flags: ReferralHeaderFlags::default(),
            entries: referral_entries,
        }
    }

    /// A cache entry whose TTL has already run out. `cache_referral` clamps
    /// a TTL to at least a second, so an expired one has to be built here.
    fn expired_entry() -> CachedReferral {
        CachedReferral {
            prefix: vec!["domain".to_string(), "dfs".to_string()],
            dfs_path_prefix: r"\\domain\dfs".to_string(),
            root_or_link: RootOrLink::Link,
            interlink: false,
            target_failback: false,
            targets: vec![DfsTarget {
                server: "srv".to_string(),
                share: "data".to_string(),
                remaining_prefix: String::new(),
            }],
            target_hint: 0,
            expires_at: Instant::now() - Duration::from_secs(1),
        }
    }

    #[test]
    fn resolver_cache_hit() {
        let mut resolver = DfsResolver::new();

        let resp = make_referral(r"\domain\dfs\docs", &[(r"\\server1\share", 600)]);
        resolver.cache_referral(r"\domain\dfs\docs", &resp);

        let result = resolver.resolve_from_cache(r"\\domain\dfs\docs\file.txt");
        assert!(result.is_some());
        let paths = result.unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].server, "server1");
        assert_eq!(paths[0].share, "share");
        assert_eq!(paths[0].port, 445);
        assert_eq!(paths[0].remaining_path, "file.txt");
    }

    #[test]
    fn resolver_cache_miss() {
        let resolver = DfsResolver::new();

        let result = resolver.resolve_from_cache(r"\\server\share\file.txt");
        assert!(result.is_none());
    }

    #[test]
    fn resolver_cache_expired() {
        let mut resolver = DfsResolver::new();

        // Insert with TTL=0 -- cache_referral clamps to 1s, so we need to
        // manually insert an already-expired entry.
        resolver
            .cache
            .insert(r"\\domain\dfs".to_string(), expired_entry());

        let result = resolver.resolve_from_cache(r"\\domain\dfs\file.txt");
        assert!(result.is_none(), "expired entry should not match");
    }

    /// A link lookup answers from a link's entry (a folder inside a share),
    /// matched case-insensitively, and counts as a cache hit.
    #[test]
    fn a_link_lookup_answers_from_a_cached_link() {
        let mut resolver = DfsResolver::new();
        let resp = make_referral(r"\srv\dfs\data", &[(r"\\fs02\files", 600)]);
        resolver.cache_referral(r"\srv\dfs\data", &resp);

        let found = resolver
            .link_from_cache(r"\\srv\dfs\DATA\Sub\f.txt")
            .expect("the link's entry covers it");
        assert_eq!(found[0].share, "files");
        assert_eq!(found[0].remaining_path, r"Sub\f.txt");
        assert_eq!(found[0].link_depth(), 1);
        assert_eq!(resolver.counters().0, 1, "a cache hit");
        assert!(resolver.link_from_cache(r"\\srv\dfs\other\f").is_none());
    }

    /// A namespace root's entry (`\\dom\ns`, from `connect_share`) covers
    /// every path under it, and isn't a link: a link lookup that took it
    /// would send the call back to the same share and be refused again.
    #[test]
    fn a_link_lookup_skips_a_root_entry() {
        let mut resolver = DfsResolver::new();
        let resp = make_referral(r"\dom\ns", &[(r"\\dom\ns", 600)]);
        resolver.cache_referral(r"\dom\ns", &resp);

        assert!(resolver.resolve_from_cache(r"\\dom\ns\data\f").is_some());
        assert!(resolver.link_from_cache(r"\\dom\ns\data\f").is_none());
    }

    /// An expired link is a miss, so the call goes back to asking the share
    /// (and its refused CREATE) rather than trusting a stale target.
    #[test]
    fn a_link_lookup_skips_an_expired_link() {
        let mut resolver = DfsResolver::new();
        let mut entry = expired_entry();
        entry.prefix.push("data".to_string());
        entry.dfs_path_prefix = r"\\domain\dfs\data".to_string();
        resolver.cache.insert(entry.dfs_path_prefix.clone(), entry);

        assert!(resolver.link_from_cache(r"\\domain\dfs\data\f").is_none());
    }

    #[test]
    fn resolver_cache_longest_prefix() {
        let mut resolver = DfsResolver::new();

        // Insert a short prefix
        let short = make_referral(r"\domain\dfs", &[(r"\\server1\root", 600)]);
        resolver.cache_referral(r"\domain\dfs", &short);

        // Insert a longer prefix
        let long = make_referral(r"\domain\dfs\docs", &[(r"\\server2\docs", 600)]);
        resolver.cache_referral(r"\domain\dfs\docs", &long);

        // Should match the longer prefix
        let result = resolver
            .resolve_from_cache(r"\\domain\dfs\docs\file.txt")
            .unwrap();
        assert_eq!(result[0].server, "server2");
        assert_eq!(result[0].share, "docs");
        assert_eq!(result[0].remaining_path, "file.txt");

        // A path that only matches the short prefix
        let result2 = resolver
            .resolve_from_cache(r"\\domain\dfs\other\file.txt")
            .unwrap();
        assert_eq!(result2[0].server, "server1");
        assert_eq!(result2[0].share, "root");
        assert_eq!(result2[0].remaining_path, r"other\file.txt");
    }

    #[test]
    fn resolver_multiple_targets() {
        let mut resolver = DfsResolver::new();

        let resp = make_referral(
            r"\domain\dfs\docs",
            &[(r"\\server1\share", 600), (r"\\server2\share", 300)],
        );
        resolver.cache_referral(r"\domain\dfs\docs", &resp);

        let result = resolver
            .resolve_from_cache(r"\\domain\dfs\docs\file.txt")
            .unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].server, "server1");
        assert_eq!(result[1].server, "server2");
        // Both should have the same remaining path
        assert_eq!(result[0].remaining_path, "file.txt");
        assert_eq!(result[1].remaining_path, "file.txt");
    }

    /// Regression: the resolver used to lowercase the whole input and slice
    /// the *lowercased* string for the remaining path, so `Docs/Report.PDF`
    /// came back as `docs\report.pdf` and opened the wrong file — or nothing
    /// at all — on a case-sensitive export. Matching stays case-insensitive,
    /// because DFS paths are; what comes back is the caller's own case.
    #[test]
    fn resolver_matches_case_insensitively_and_preserves_the_callers_case() {
        let mut resolver = DfsResolver::new();

        let resp = make_referral(r"\domain\dfs\docs", &[(r"\\server\share", 600)]);
        resolver.cache_referral(r"\domain\dfs\docs", &resp);

        // Resolve with double-backslash prefix and mixed case
        let result = resolver
            .resolve_from_cache(r"\\DOMAIN\DFS\DOCS\Sub\File.txt")
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].server, "server");
        assert_eq!(result[0].share, "share");
        assert_eq!(result[0].remaining_path, r"Sub\File.txt");

        // Forward slashes should also work
        let result2 = resolver
            .resolve_from_cache(r"\\domain/dfs/docs/Other.TXT")
            .unwrap();
        assert_eq!(result2[0].remaining_path, "Other.TXT");
    }

    /// MS-DFSC § 3.1.4.1: "Whole path components are used in the match." A
    /// raw `starts_with` let `\\dom\ns` swallow `\\dom\nsfoo\x` and hand the
    /// caller a path on a server that knows nothing about it.
    #[test]
    fn resolver_matches_only_on_component_boundaries() {
        let mut resolver = DfsResolver::new();

        let resp = make_referral(r"\dom\ns", &[(r"\\server\share", 600)]);
        resolver.cache_referral(r"\dom\ns", &resp);

        assert!(resolver.resolve_from_cache(r"\\dom\nsfoo\x").is_none());
        assert!(resolver.resolve_from_cache(r"\\dom\nsfoo").is_none());
        // The prefix itself, and anything under it, still match.
        assert!(resolver.resolve_from_cache(r"\\dom\ns").is_some());
        assert!(resolver.resolve_from_cache(r"\\dom\ns\foo").is_some());
    }

    /// A V1 entry has no `DFSPath`, so its cache key comes from
    /// `PathConsumed` against the path we asked about.
    #[test]
    fn resolver_keys_a_v1_referral_from_path_consumed() {
        let mut resolver = DfsResolver::new();

        let request = r"\dom\ns\dir\file.txt";
        // `\dom\ns` is 7 UTF-16 code units, so 14 bytes.
        let resp = RespGetDfsReferral {
            path_consumed: 14,
            header_flags: ReferralHeaderFlags::new(ReferralHeaderFlags::STORAGE_SERVERS),
            entries: vec![DfsReferralEntry::V1 {
                server_type: 1,
                share_name: r"\\fs01\ns_root".to_string(),
            }],
        };
        resolver.cache_referral(request, &resp);

        let resolved = resolver
            .resolve_from_cache(r"\\dom\ns\dir\file.txt")
            .expect("V1 referral should be cached under the consumed prefix");
        assert_eq!(resolved[0].server, "fs01");
        assert_eq!(resolved[0].share, "ns_root");
        assert_eq!(resolved[0].remaining_path, r"dir\file.txt");
    }

    /// `PathConsumed` counts bytes of UTF-16LE. An odd count, a zero, or one
    /// past the end of the path means the two sides disagree about the path,
    /// and guessing at it would cache a prefix nothing asked for.
    #[test]
    fn path_consumed_prefix_rejects_nonsense() {
        let path = r"\dom\ns\dir";
        assert_eq!(prefix_from_path_consumed(path, 14), Some(r"\dom\ns"));
        assert_eq!(prefix_from_path_consumed(path, 22), Some(path));
        assert_eq!(prefix_from_path_consumed(path, 0), None);
        assert_eq!(prefix_from_path_consumed(path, 13), None); // odd
        assert_eq!(prefix_from_path_consumed(path, 24), None); // past the end

        // Astral characters are two code units each, so a count landing
        // inside a surrogate pair names no prefix at all.
        let emoji = r"\a\🦀x";
        assert_eq!(prefix_from_path_consumed(emoji, 6), Some(r"\a\"));
        assert_eq!(prefix_from_path_consumed(emoji, 8), None); // splits the pair
        assert_eq!(prefix_from_path_consumed(emoji, 10), Some(r"\a\🦀"));
    }

    /// A V1 response whose `PathConsumed` names no prefix is not cached, and
    /// that is quieter than caching something arbitrary.
    #[test]
    fn resolver_skips_a_v1_referral_it_cannot_key() {
        let mut resolver = DfsResolver::new();
        let resp = RespGetDfsReferral {
            path_consumed: 9999,
            header_flags: ReferralHeaderFlags::default(),
            entries: vec![DfsReferralEntry::V1 {
                server_type: 1,
                share_name: r"\\fs01\ns_root".to_string(),
            }],
        };
        resolver.cache_referral(r"\dom\ns", &resp);
        assert!(resolver.cache.is_empty());
    }

    /// § 3.1.1 TargetHint: once a target works, it is tried first, so a
    /// failover is not re-walked on every later lookup.
    #[test]
    fn resolver_remembers_the_target_that_worked() {
        let mut resolver = DfsResolver::new();

        let resp = make_referral(
            r"\dom\ns",
            &[(r"\\dead\share", 600), (r"\\live\share", 600)],
        );
        resolver.cache_referral(r"\dom\ns", &resp);

        let first = resolver.resolve_from_cache(r"\\dom\ns\f.txt").unwrap();
        assert_eq!(first[0].server, "dead");

        // The second one is what actually connected.
        resolver.note_target_worked(&first[1]);

        let second = resolver.resolve_from_cache(r"\\dom\ns\f.txt").unwrap();
        assert_eq!(second[0].server, "live");
        // The dead one is still offered, just not first.
        assert_eq!(second.len(), 2);
        assert_eq!(second[1].server, "dead");
    }

    /// The cache is keyed by server-supplied strings, so it is bounded.
    #[test]
    fn resolver_cache_is_bounded() {
        let mut resolver = DfsResolver::new();

        for i in 0..(MAX_CACHE_ENTRIES + 50) {
            let dfs_path = format!(r"\dom\ns{i}");
            let resp = make_referral(&dfs_path, &[(r"\\server\share", 600)]);
            resolver.cache_referral(&dfs_path, &resp);
        }

        assert!(
            resolver.cache.len() <= MAX_CACHE_ENTRIES,
            "cache grew to {}",
            resolver.cache.len()
        );
    }

    /// Expired entries are evicted before live ones.
    #[test]
    fn resolver_evicts_expired_entries_first() {
        let mut resolver = DfsResolver::new();

        for i in 0..MAX_CACHE_ENTRIES {
            resolver
                .cache
                .insert(format!(r"\\dead\ns{i}"), expired_entry());
        }
        let resp = make_referral(r"\dom\fresh", &[(r"\\server\share", 600)]);
        resolver.cache_referral(r"\dom\fresh", &resp);

        assert_eq!(resolver.cache.len(), 1);
        assert!(resolver.resolve_from_cache(r"\\dom\fresh\x").is_some());
    }

    /// The referral header's Interlink and TargetFailback bits are evaluated
    /// once at insert and carried on every resolved path.
    #[test]
    fn resolver_stores_header_bits() {
        let mut resolver = DfsResolver::new();

        let mut resp = make_referral(r"\dom\ns", &[(r"\\other\share", 600)]);
        resp.header_flags = ReferralHeaderFlags::new(ReferralHeaderFlags::REFERRAL_SERVERS);
        resolver.cache_referral(r"\dom\ns", &resp);

        let entry = &resolver.cache[r"\\dom\ns"];
        assert!(entry.interlink);
        assert!(!entry.target_failback);
        // `make_referral` builds `server_type = 0`, which is what Samba
        // answers for a root. It is stored, and gates nothing.
        assert_eq!(entry.root_or_link, RootOrLink::Link);

        // And all of it reaches diagnostics, which is where someone debugging
        // a namespace looks.
        let snapshot = &resolver.cache_entries()[0];
        assert!(snapshot.interlink);
        assert!(!snapshot.root_targets);
        assert!(!snapshot.target_failback);
        assert_eq!(snapshot.target_hint, 0);
    }

    #[test]
    fn resolver_remaining_prefix_from_target() {
        let mut resolver = DfsResolver::new();

        // Target has a remaining prefix (network_address includes a subpath)
        let resp = make_referral(r"\domain\dfs\docs", &[(r"\\server\share\subdir", 600)]);
        resolver.cache_referral(r"\domain\dfs\docs", &resp);

        // With additional path after the DFS prefix
        let result = resolver
            .resolve_from_cache(r"\\domain\dfs\docs\file.txt")
            .unwrap();
        assert_eq!(result[0].remaining_path, r"subdir\file.txt");

        // Without additional path -- just the target's remaining prefix
        let result2 = resolver.resolve_from_cache(r"\\domain\dfs\docs").unwrap();
        assert_eq!(result2[0].remaining_path, "subdir");
    }
}

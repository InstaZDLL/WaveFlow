//! Every connection an [`SmbClient`](super::SmbClient) holds, and the one way
//! to reach the right one for a [`Tree`].
//!
//! A `TreeId` only means something inside the session that issued it, and a
//! tree DFS moved to another server lives on that server's connection. Sent
//! over the wrong one, it either fails with `STATUS_NETWORK_NAME_DELETED` or,
//! when the ids collide, reads or writes a same-named file on a different
//! share. So the primary connection sits behind this type: a method holding a
//! `Tree` asks [`Connections::for_tree`], and reaching the primary directly
//! takes an explicit [`Connections::primary_mut`], which only calls that have
//! no tree (share listing, the namespace root's own tree connect, reconnect)
//! have any business with.

use std::collections::HashMap;

use log::debug;

use crate::client::connection::Connection;
use crate::client::session::Session;
use crate::client::tree::Tree;
use crate::error::Result;
use crate::Error;

/// A connection to a specific server with its authenticated session.
///
/// Used for DFS cross-server referrals where the client needs connections
/// to multiple servers simultaneously.
pub(crate) struct ConnectionEntry {
    /// The connection to the server.
    pub conn: Connection,
    /// The session as of the last authentication on this connection. Behind
    /// an `Arc` for the same reason the primary's is: a revival can establish
    /// a new one behind this client's back, and share encryption derives from
    /// the live keys.
    pub session: std::sync::Arc<Session>,
}

/// The primary connection plus the pool of extra connections for DFS targets,
/// keyed by `host:port`.
///
/// The pool never replaces the primary: that would invalidate every `Tree`
/// the caller already holds.
pub(crate) struct Connections {
    primary: Connection,
    /// `addr:port` of the primary, which is what a tree on it carries in
    /// [`Tree::server`].
    primary_server: String,
    extra: HashMap<String, ConnectionEntry>,
    /// The target shares DFS links led to, for the calls that follow a link
    /// without handing the caller a tree: keyed by `addr:port` and lowercased
    /// share name, stamped with the connection's generation.
    link_trees: HashMap<(String, String), (u64, Tree)>,
}

impl Connections {
    pub(crate) fn new(primary: Connection, primary_server: String) -> Self {
        Connections {
            primary,
            primary_server,
            extra: HashMap::new(),
            link_trees: HashMap::new(),
        }
    }

    /// The tree a DFS link already led to on `addr`'s `share`, if it's still
    /// good.
    ///
    /// ❌ **One per target share, never one per call.** Every `download` or
    /// writer through a link would otherwise TREE_CONNECT again and never
    /// disconnect, since the handle holding the tree can't. The generation
    /// check drops a tree whose connection came back on a new session, where
    /// its id means nothing, and a connection that's dead for good isn't handed
    /// out at all, or a target that dropped would stay dead for the client's
    /// whole life.
    pub(crate) fn link_tree(&self, addr: &str, share: &str) -> Option<Tree> {
        let (generation, tree) = self.link_trees.get(&link_key(addr, share))?;
        let conn = self.for_tree_ref(tree).ok()?;
        (conn.generation() == *generation && !dead_for_good(conn)).then(|| tree.clone())
    }

    /// Whether `tree` is a target a DFS link led to, which is what makes its
    /// connection's loss the link's rather than the share's.
    pub(crate) fn is_link_tree(&self, tree: &Tree) -> bool {
        tree.server != self.primary_server
            && self
                .link_trees
                .values()
                .any(|(_, known)| known.server == tree.server && known.tree_id == tree.tree_id)
    }

    /// Let go of a link target's connection that was lost, so the next call
    /// through the link dials afresh. One a reviver can bring back stays: it
    /// may carry a namespace root's traffic too, and its trees heal in place.
    pub(crate) fn forget_lost_target(&mut self, addr: &str) {
        if self
            .extra
            .get(addr)
            .is_some_and(|entry| !entry.conn.can_reconnect())
        {
            self.drop_target(addr);
        }
    }

    /// [`forget_lost_target`](Self::forget_lost_target) for a dial about to
    /// reuse the pool: only a connection that's already dead for good goes.
    pub(crate) fn forget_dead_target(&mut self, addr: &str) {
        if self
            .extra
            .get(addr)
            .is_some_and(|entry| dead_for_good(&entry.conn))
        {
            self.drop_target(addr);
        }
    }

    fn drop_target(&mut self, addr: &str) {
        self.extra.remove(addr);
        self.link_trees.retain(|_, (_, known)| known.server != addr);
    }

    /// Remember the tree a DFS link led to, for [`link_tree`](Self::link_tree).
    pub(crate) fn remember_link_tree(&mut self, tree: &Tree) {
        let Ok(conn) = self.for_tree_ref(tree) else {
            return;
        };
        let generation = conn.generation();
        self.link_trees.insert(
            link_key(&tree.server, &tree.share_name),
            (generation, tree.clone()),
        );
    }

    /// Stop handing out `tree`, because its tree connect is being torn down.
    pub(crate) fn forget_link_tree(&mut self, tree: &Tree) {
        self.link_trees
            .retain(|_, (_, known)| known.server != tree.server || known.tree_id != tree.tree_id);
    }

    /// The connection `tree` lives on.
    ///
    /// ❌ **`Error::Disconnected`, never a panic.** A `Tree` outlives the pool
    /// entry it was resolved on: a reconnect drops every extra connection, and
    /// a consumer reasonably still holds the DFS-resolved `Tree` it had.
    /// `Disconnected` is both true and the classification whose documented
    /// response (`connect_share` again) is exactly right here.
    pub(crate) fn for_tree(&mut self, tree: &Tree) -> Result<&mut Connection> {
        if tree.server == self.primary_server {
            return Ok(&mut self.primary);
        }
        match self.extra.get_mut(&tree.server) {
            Some(entry) => Ok(&mut entry.conn),
            None => Err(orphaned(tree)),
        }
    }

    /// [`for_tree`](Self::for_tree) for a caller that only has `&self`, such
    /// as the handle openers, which clone the connection they get.
    pub(crate) fn for_tree_ref(&self, tree: &Tree) -> Result<&Connection> {
        if tree.server == self.primary_server {
            return Ok(&self.primary);
        }
        match self.extra.get(&tree.server) {
            Some(entry) => Ok(&entry.conn),
            None => Err(orphaned(tree)),
        }
    }

    /// The connection to `addr`, for a DFS referral that names a server
    /// rather than a tree.
    pub(crate) fn for_addr(&mut self, addr: &str) -> Result<&mut Connection> {
        if self.is_primary(addr) {
            return Ok(&mut self.primary);
        }
        self.extra
            .get_mut(addr)
            .map(|entry| &mut entry.conn)
            .ok_or(Error::Disconnected)
    }

    /// The primary connection, for the calls that have no tree to route by.
    pub(crate) fn primary(&self) -> &Connection {
        &self.primary
    }

    /// The primary connection, mutably, for the calls that have no tree to
    /// route by. ❌ Never for a call that has one: use
    /// [`for_tree`](Self::for_tree).
    pub(crate) fn primary_mut(&mut self) -> &mut Connection {
        &mut self.primary
    }

    /// `addr:port` of the primary connection.
    pub(crate) fn primary_server(&self) -> &str {
        &self.primary_server
    }

    pub(crate) fn is_primary(&self, addr: &str) -> bool {
        addr == self.primary_server
    }

    pub(crate) fn extra(&self, addr: &str) -> Option<&ConnectionEntry> {
        self.extra.get(addr)
    }

    pub(crate) fn extra_mut(&mut self, addr: &str) -> Option<&mut ConnectionEntry> {
        self.extra.get_mut(addr)
    }

    pub(crate) fn extras(&self) -> impl Iterator<Item = &ConnectionEntry> {
        self.extra.values()
    }

    pub(crate) fn insert_extra(&mut self, addr: String, entry: ConnectionEntry) {
        self.extra.insert(addr, entry);
    }

    /// Drop every DFS connection. Trees resolved on them get
    /// `Error::Disconnected` from here on.
    pub(crate) fn clear_extras(&mut self) {
        self.extra.clear();
        self.link_trees.clear();
    }
}

/// A connection that's down with no reviver to bring it back.
fn dead_for_good(conn: &Connection) -> bool {
    conn.is_disconnected() && !conn.can_reconnect()
}

/// Share names are case-insensitive on every SMB server.
fn link_key(addr: &str, share: &str) -> (String, String) {
    (addr.to_string(), share.to_lowercase())
}

fn orphaned(tree: &Tree) -> Error {
    debug!(
        "smb_client: no connection for this tree; the DFS target it was resolved on is \
         gone, so connect_share again server={:?} share={:?}",
        tree.server, tree.share_name
    );
    Error::Disconnected
}

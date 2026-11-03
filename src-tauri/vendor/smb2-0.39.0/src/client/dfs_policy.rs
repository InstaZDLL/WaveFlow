//! Whether a client may sign in to a server a DFS referral named.
//!
//! A referral comes from the server, and a client that follows one dials the
//! host it names and signs in there with the credentials it holds. So a
//! malicious or compromised share can point a link at its own machine and
//! capture that sign-in. [`DfsTargetPolicy`] is what stands in between, and
//! `SmbClient`'s DFS dial, its pooled-target lookup, and a target's reviver all
//! ask it before they send anything. See `client/CLAUDE.md` § DFS.

use std::net::IpAddr;
use std::sync::Arc;

/// The default SMB port, which an address with no port means.
const DEFAULT_SMB_PORT: u16 = 445;

/// Who decides whether this client may sign in to a server a DFS referral
/// named, when the two aren't provably the same server.
///
/// Set it on [`ClientConfig::dfs_target_policy`](super::ClientConfig::dfs_target_policy).
/// The default signs in nowhere new.
#[derive(Clone, Default)]
pub enum DfsTargetPolicy {
    /// Follow a referral only to the client's own server. Anything else is
    /// [`Error::DfsTargetNotAllowed`](crate::Error::DfsTargetNotAllowed).
    #[default]
    SameServerOnly,
    /// Follow every referral, signing in wherever it says with the client's
    /// credentials. ❗ A server can then collect those credentials by naming a
    /// machine of its own, so use this only where every server is trusted.
    FollowAll,
    /// Ask for each [`DfsSignIn`], every time, before anything goes on the
    /// wire. `false` is [`Error::DfsTargetNotAllowed`](crate::Error::DfsTargetNotAllowed).
    /// Runs inside the client's call, so answer from memory: ❌ don't block on
    /// a person here. Refuse, and ask them when the error comes back.
    Ask(Arc<dyn Fn(&DfsSignIn) -> bool + Send + Sync>),
}

impl std::fmt::Debug for DfsTargetPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SameServerOnly => "SameServerOnly",
            Self::FollowAll => "FollowAll",
            Self::Ask(_) => "Ask",
        })
    }
}

/// A sign-in a DFS referral would cause on a different server.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DfsSignIn {
    /// The host this client connected to, compared form: lowercase, with no
    /// trailing dot or port.
    pub server: String,
    /// The host the referral named, in the same form.
    pub target: String,
}

impl DfsTargetPolicy {
    /// Whether the client connected to `primary_addr` may dial `target_addr`,
    /// which a referral named `named`. `Err` carries the sign-in it refused.
    ///
    /// `target_addr` is what gets dialed, so it's what "the same server" is
    /// judged on (a consumer's `dfs_target_overrides` map to it); `named` is what
    /// a person or a consent record knows the target by.
    pub(crate) fn check(
        &self,
        primary_addr: &str,
        named: &str,
        target_addr: &str,
    ) -> std::result::Result<(), DfsSignIn> {
        if same_server(primary_addr, target_addr) {
            return Ok(());
        }
        let sign_in = DfsSignIn {
            server: compared_host(primary_addr),
            target: compared_name(named),
        };
        let allowed = match self {
            Self::SameServerOnly => false,
            Self::FollowAll => true,
            Self::Ask(ask) => ask(&sign_in),
        };
        if allowed {
            Ok(())
        } else {
            Err(sign_in)
        }
    }
}

/// Whether two `host:port` addresses provably name the same server: the same
/// host once case, one trailing dot, and an IP literal's spelling are set aside,
/// and the same port.
///
/// ❌ **Anything else is "not the same", including everything unsure.** A short
/// name and its FQDN, a name and its IP, and an address this can't read all
/// could be one machine, but nothing here can prove it, and a wrong "same" hands
/// a password to a stranger.
pub(crate) fn same_server(a: &str, b: &str) -> bool {
    match (split_addr(a), split_addr(b)) {
        (Some((host_a, port_a)), Some((host_b, port_b))) => {
            !host_a.is_empty() && host_a == host_b && port_a == port_b
        }
        _ => false,
    }
}

/// `host:port` read strictly, with the host in compared form: `None` for
/// anything that isn't exactly a name, an IPv4 address, or a bracketed IPv6
/// address, an optional port after it. A bare IPv6 address also reads, since it
/// can't be anything else.
fn split_addr(addr: &str) -> Option<(String, u16)> {
    if let Some(rest) = addr.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let ip = host.parse::<std::net::Ipv6Addr>().ok()?;
        let port = match after {
            "" => DEFAULT_SMB_PORT,
            _ => after.strip_prefix(':')?.parse().ok()?,
        };
        return Some((canonical_ip(IpAddr::V6(ip)), port));
    }
    if let Ok(ip) = addr.parse::<IpAddr>() {
        return Some((canonical_ip(ip), DEFAULT_SMB_PORT));
    }
    let (host, port) = match addr.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().ok()?),
        None => (addr, DEFAULT_SMB_PORT),
    };
    if host.contains(':') || host.contains('[') || host.contains(']') {
        return None;
    }
    Some((compared_name(host), port))
}

/// A host name in compared form. An IP literal gets its one spelling. Only
/// ASCII folds: whether two other letters are "the same" is a question for the
/// name server, and an unsure answer means different.
fn compared_name(name: &str) -> String {
    if let Ok(ip) = name.parse::<IpAddr>() {
        return canonical_ip(ip);
    }
    let name = name.strip_suffix('.').unwrap_or(name);
    name.to_ascii_lowercase()
}

/// The compared form of an address's host, for naming it: its parsed host where
/// it reads, else the whole thing, never a guess at a shorter one.
fn compared_host(addr: &str) -> String {
    split_addr(addr).map_or_else(|| compared_name(addr), |(host, _)| host)
}

fn canonical_ip(ip: IpAddr) -> String {
    ip.to_canonical().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_is_the_same_server_across_case_a_trailing_dot_and_a_missing_port() {
        assert!(same_server("nas.local:445", "NAS.Local:445"));
        assert!(same_server("nas.local:445", "nas.local.:445"));
        assert!(same_server("nas.local", "nas.local:445"));
        assert!(same_server("10.0.0.1:445", "10.0.0.1"));
    }

    #[test]
    fn an_ip_literal_is_the_same_server_in_every_spelling() {
        assert!(same_server("[::1]:445", "[0:0:0:0:0:0:0:1]:445"));
        assert!(same_server("[::1]:445", "::1"));
        assert!(same_server("[FE80::1]:445", "[fe80::1]:445"));
        assert!(same_server("[::ffff:10.0.0.1]:445", "10.0.0.1:445"));
    }

    #[test]
    fn what_cant_be_proven_the_same_is_different() {
        assert!(!same_server("nas:445", "nas.local:445"), "short name, FQDN");
        assert!(!same_server("10.0.0.1:445", "nas:445"), "IP, name");
        assert!(!same_server("nas:445", "nas:10445"), "another port");
        assert!(!same_server("nas:445", "other:445"));
        assert!(!same_server("nas:445", "nas..:445"), "one dot only");
        assert!(!same_server(":445", ":445"), "no host names no server");
        assert!(
            !same_server("nas:445", "[nas]evil.example:445"),
            "a bracket must not hide the real host"
        );
        assert!(!same_server("nas:445", "nas:445:445"));
        assert!(!same_server("nas:445", "nas:notaport"));
        // Only ASCII folds, so a lookalike can't pass as the same name.
        assert!(!same_server("nas:445", "nаs:445"), "Cyrillic а");
        assert!(!same_server("straße:445", "STRASSE:445"));
    }

    #[test]
    fn the_same_server_is_never_asked_about() {
        let policy = DfsTargetPolicy::Ask(Arc::new(|_| panic!("nothing to ask")));
        assert_eq!(policy.check("nas:445", "NAS.", "NAS.:445"), Ok(()));
    }

    #[test]
    fn the_default_policy_refuses_a_different_server_and_names_both() {
        let refused = DfsTargetPolicy::default()
            .check("NAS.:445", "Evil.Example.", "Evil.Example.:445")
            .unwrap_err();
        assert_eq!(
            refused,
            DfsSignIn {
                server: "nas".to_string(),
                target: "evil.example".to_string(),
            }
        );
    }

    #[test]
    fn asking_decides_on_the_pair() {
        let policy = DfsTargetPolicy::Ask(Arc::new(|sign_in| {
            sign_in.server == "nas" && sign_in.target == "fs01"
        }));
        assert_eq!(policy.check("nas:445", "FS01", "FS01:445"), Ok(()));
        assert!(policy.check("nas:445", "fs02", "fs02:445").is_err());
        assert!(policy.check("other:445", "fs01", "fs01:445").is_err());
    }

    #[test]
    fn follow_all_follows() {
        assert_eq!(
            DfsTargetPolicy::FollowAll.check("nas:445", "fs01", "fs01:445"),
            Ok(())
        );
    }
}

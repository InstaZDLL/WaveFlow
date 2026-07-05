//! Local-vs-remote digest comparison.
//!
//! Inputs are a [`super::LocalDigest`] (computed by
//! [`super::read_local_digest`]) and a [`super::client::RemoteDigest`]
//! (fetched by [`super::client::fetch_remote_digest`]). Output is a
//! [`DigestDiff`] flagging the rows the two replicas disagree on.
//!
//! ## Algorithm
//!
//! 1. **Fast path** — compare `set_hash` (hex). Equal hashes mean
//!    the two sets are identical at the member level. The diff
//!    short-circuits to "in sync" without walking the lists.
//! 2. **Member sweep** — both lists are sorted by `canonical_id`
//!    ASC (the server's `ORDER BY canonical_id` and our matching
//!    `read_local_digest` queries guarantee it). Two-pointer merge
//!    classifies each canonical_id as:
//!    - `MissingLocally` — present on the server, absent locally.
//!      The backfill (Phase B.2) will pull these.
//!    - `MissingRemotely` — present locally, absent on the server.
//!      The backfill will push these.
//!    - `Divergent` — same canonical_id, different `payload_hash`.
//!      The backfill picks the LWW winner via §2 total order
//!      (HLC + origin_device_id, not in this struct yet).
//!
//! The version + max_hlc deltas ride alongside for the Settings
//! UI status surface (Phase B.3).

use serde::Serialize;

use super::client::{RemoteDigest, RemoteMember};
use super::LocalDigest;

/// Classification of one canonical_id-keyed disagreement.
#[derive(Debug, Clone, Serialize)]
pub struct DivergentMember {
    pub canonical_id: String,
    /// Hex-encoded local `payload_hash`. Empty when the member is
    /// absent locally (the variant communicates direction; the
    /// hash field stays present for a uniform JSON shape consumed
    /// by the Settings UI).
    pub local_payload_hash: String,
    /// Hex-encoded remote `payload_hash`. Empty when absent remotely.
    pub remote_payload_hash: String,
}

/// Summary returned by [`diff`]. Empty `Vec`s + `in_sync = true`
/// when the two replicas agree.
#[derive(Debug, Clone, Serialize)]
pub struct DigestDiff {
    pub entity: String,
    pub in_sync: bool,
    pub local_version: i64,
    pub remote_version: i64,
    /// `(canonical_id, remote_payload_hash)` pairs the server has
    /// but the desktop doesn't.
    pub missing_locally: Vec<RemoteMember>,
    /// `(canonical_id, local_payload_hash hex)` pairs the desktop
    /// has but the server doesn't.
    pub missing_remotely: Vec<DivergentMember>,
    /// Same canonical_id, different payload_hash.
    pub divergent: Vec<DivergentMember>,
}

/// Diff a local digest against the matching server response.
///
/// `local.entity` is echoed onto the output; the function does not
/// re-validate that `remote` corresponds to the same entity
/// (the caller fetches one after computing the other against the
/// same `entity` arg).
pub fn diff(local: &LocalDigest, remote: &RemoteDigest) -> DigestDiff {
    let local_set_hex = hex::encode(local.set_hash);
    if local_set_hex == remote.set_hash {
        return DigestDiff {
            entity: local.entity.clone(),
            in_sync: true,
            local_version: local.version,
            remote_version: remote.version,
            missing_locally: Vec::new(),
            missing_remotely: Vec::new(),
            divergent: Vec::new(),
        };
    }

    let mut missing_locally = Vec::new();
    let mut missing_remotely = Vec::new();
    let mut divergent = Vec::new();

    let mut li = 0usize;
    let mut ri = 0usize;
    let locals = &local.members;
    let remotes = &remote.members;

    while li < locals.len() && ri < remotes.len() {
        let l = &locals[li];
        let r = &remotes[ri];
        match l.canonical_id.as_str().cmp(r.canonical_id.as_str()) {
            std::cmp::Ordering::Equal => {
                let local_hex = hex::encode(&l.payload_hash);
                if local_hex != r.payload_hash {
                    divergent.push(DivergentMember {
                        canonical_id: l.canonical_id.clone(),
                        local_payload_hash: local_hex,
                        remote_payload_hash: r.payload_hash.clone(),
                    });
                }
                li += 1;
                ri += 1;
            }
            std::cmp::Ordering::Less => {
                // Local has a canonical the server doesn't — push
                // candidate.
                missing_remotely.push(DivergentMember {
                    canonical_id: l.canonical_id.clone(),
                    local_payload_hash: hex::encode(&l.payload_hash),
                    remote_payload_hash: String::new(),
                });
                li += 1;
            }
            std::cmp::Ordering::Greater => {
                missing_locally.push(r.clone());
                ri += 1;
            }
        }
    }
    while li < locals.len() {
        let l = &locals[li];
        missing_remotely.push(DivergentMember {
            canonical_id: l.canonical_id.clone(),
            local_payload_hash: hex::encode(&l.payload_hash),
            remote_payload_hash: String::new(),
        });
        li += 1;
    }
    while ri < remotes.len() {
        missing_locally.push(remotes[ri].clone());
        ri += 1;
    }

    DigestDiff {
        entity: local.entity.clone(),
        in_sync: false,
        local_version: local.version,
        remote_version: remote.version,
        missing_locally,
        missing_remotely,
        divergent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::digest::LocalMember;

    fn local(entity: &str, members: Vec<(&str, &str)>, version: i64) -> LocalDigest {
        let members: Vec<LocalMember> = members
            .into_iter()
            .map(|(id, hash_hex)| LocalMember {
                canonical_id: id.to_string(),
                payload_hash: hex::decode(hash_hex).unwrap(),
            })
            .collect();
        // Compute set_hash from members so the equality fast-path
        // mirrors what `read_local_digest` would produce.
        let pairs: Vec<(&str, &[u8])> = members
            .iter()
            .map(|m| (m.canonical_id.as_str(), m.payload_hash.as_slice()))
            .collect();
        let set_hash = waveflow_core::sync::digest::compute_set_hash(&pairs);
        LocalDigest {
            entity: entity.to_string(),
            set_hash,
            version,
            max_hlc: None,
            members,
        }
    }

    fn remote(set_hash: &str, members: Vec<(&str, &str)>, version: i64) -> RemoteDigest {
        RemoteDigest {
            set_hash: set_hash.to_string(),
            version,
            max_hlc: None,
            members: members
                .into_iter()
                .map(|(id, hash)| RemoteMember {
                    canonical_id: id.to_string(),
                    payload_hash: hash.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn matching_set_hash_short_circuits_to_in_sync() {
        let l = local("library", vec![("aaa", "0011223344")], 1);
        let r = remote(&hex::encode(l.set_hash), vec![("aaa", "0011223344")], 1);
        let d = diff(&l, &r);
        assert!(d.in_sync);
        assert!(d.missing_locally.is_empty());
        assert!(d.missing_remotely.is_empty());
        assert!(d.divergent.is_empty());
        assert_eq!(d.local_version, 1);
        assert_eq!(d.remote_version, 1);
    }

    #[test]
    fn missing_locally_flagged_when_server_has_extra() {
        // Local empty, server has one row.
        let l = local("library", vec![], 0);
        let r = remote("ffff", vec![("aaa", "00aa")], 1);
        let d = diff(&l, &r);
        assert!(!d.in_sync);
        assert_eq!(d.missing_locally.len(), 1);
        assert_eq!(d.missing_locally[0].canonical_id, "aaa");
        assert!(d.missing_remotely.is_empty());
        assert!(d.divergent.is_empty());
    }

    #[test]
    fn missing_remotely_flagged_when_desktop_has_extra() {
        let l = local("library", vec![("zzz", "00bb")], 1);
        let r = remote("ffff", vec![], 0);
        let d = diff(&l, &r);
        assert!(!d.in_sync);
        assert_eq!(d.missing_remotely.len(), 1);
        assert_eq!(d.missing_remotely[0].canonical_id, "zzz");
        assert!(d.missing_locally.is_empty());
        assert!(d.divergent.is_empty());
    }

    #[test]
    fn divergent_flagged_when_same_id_different_hash() {
        let l = local("library", vec![("aaa", "0011")], 1);
        let r = remote("ffff", vec![("aaa", "0099")], 1);
        let d = diff(&l, &r);
        assert!(!d.in_sync);
        assert_eq!(d.divergent.len(), 1);
        assert_eq!(d.divergent[0].canonical_id, "aaa");
        assert_eq!(d.divergent[0].local_payload_hash, "0011");
        assert_eq!(d.divergent[0].remote_payload_hash, "0099");
    }

    #[test]
    fn mixed_diff_correctly_classifies_each_canonical() {
        // local: {alpha:01, gamma:02, mike:03}
        // remote: {beta:0b, gamma:0c, mike:03, zulu:0d}
        // → divergent: gamma (02 vs 0c)
        //   in sync: mike (03 == 03)
        //   missing_remotely: alpha
        //   missing_locally: beta, zulu
        let l = local(
            "library",
            vec![("alpha", "01"), ("gamma", "02"), ("mike", "03")],
            5,
        );
        let r = remote(
            "ffff",
            vec![
                ("beta", "0b"),
                ("gamma", "0c"),
                ("mike", "03"),
                ("zulu", "0d"),
            ],
            7,
        );
        let d = diff(&l, &r);
        assert!(!d.in_sync);
        assert_eq!(d.local_version, 5);
        assert_eq!(d.remote_version, 7);

        let mr_ids: Vec<&str> = d
            .missing_remotely
            .iter()
            .map(|m| m.canonical_id.as_str())
            .collect();
        assert_eq!(mr_ids, vec!["alpha"]);

        let ml_ids: Vec<&str> = d
            .missing_locally
            .iter()
            .map(|m| m.canonical_id.as_str())
            .collect();
        assert_eq!(ml_ids, vec!["beta", "zulu"]);

        assert_eq!(d.divergent.len(), 1);
        assert_eq!(d.divergent[0].canonical_id, "gamma");
    }

    #[test]
    fn matching_members_with_mismatched_set_hash_still_diff_clean() {
        // Edge: somehow set_hash mismatch (corruption / wire bug)
        // but the per-row data agrees. The slow path still walks
        // and finds zero disagreements; in_sync stays false to
        // surface the metadata-level drift to the operator.
        let l = local("library", vec![("aaa", "01")], 1);
        let r = remote("ffff", vec![("aaa", "01")], 1);
        let d = diff(&l, &r);
        assert!(!d.in_sync);
        assert!(d.missing_locally.is_empty());
        assert!(d.missing_remotely.is_empty());
        assert!(d.divergent.is_empty());
    }
}

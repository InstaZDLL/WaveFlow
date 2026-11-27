use super::*;

mod clock;
mod frame;
mod host;
mod replica;
mod wire;

/// An entry with id `[n; 16]`, well-formed, owned by nobody yet (the host
/// sets the owner).
fn entry(n: u8) -> Entry {
    Entry {
        id: id(n),
        owner: ParticipantId(0),
        title: format!("Track {n}"),
        artists: vec!["Artist".into()],
        album: Some("Album".into()),
        duration_ms: 180_000,
        codec: "flac".into(),
        sample_rate: 44_100,
        channels: 2,
        size: 30_000_000,
        full_hash: None,
        gain: None,
    }
}

fn id(n: u8) -> EntryId {
    EntryId::from_bytes([n; 16])
}

/// A session hosted by `p1`, with one guest `p2` admitted.
fn session() -> (Host, ParticipantId, ParticipantId) {
    let mut host = Host::new("Friday", "Alex", 1_000).unwrap();
    let (guest, _) = host.admit("Sam").unwrap();
    (host.clone(), host.host_id(), guest)
}

fn add(host: &mut Host, from: ParticipantId, ns: &[u8]) {
    host.handle(
        from,
        Action::Add {
            entries: ns.iter().map(|&n| entry(n)).collect(),
            position: Position::End,
        },
        1_000,
    )
    .unwrap();
}

fn ids(entries: &[Entry]) -> Vec<EntryId> {
    entries.iter().map(|e| e.id).collect()
}

#[test]
fn an_entry_id_reads_back_only_in_its_one_spelling() {
    let id = EntryId::from_bytes([0xab; 16]);
    let written = id.to_string();
    assert_eq!(written, format!("wse_{}", "ab".repeat(16)));
    assert_eq!(written.parse::<EntryId>(), Ok(id));
    assert_eq!(format!("{id:?}"), written);

    for bad in [
        "",
        "wse_",
        &format!("wse_{}", "AB".repeat(16)),
        &format!("ge_{}", "ab".repeat(16)),
        &format!("wse_{}", "ab".repeat(15)),
        &format!("wse_{}0", "ab".repeat(16)),
        &format!("wse_{}zz", "ab".repeat(15)),
        &format!("wse_/{}", "ab".repeat(15)),
    ] {
        assert_eq!(bad.parse::<EntryId>(), Err(BadEntryId), "{bad:?}");
    }

    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(serde_json::from_str::<EntryId>(&json).unwrap(), id);
    assert!(serde_json::from_str::<EntryId>("\"wse_12\"").is_err());
}

#[test]
fn a_secret_is_never_printed() {
    let secret = Secret::new("wst_hunter2");
    assert_eq!(format!("{secret:?}"), "Secret(***)");
    assert_eq!(secret.expose(), "wst_hunter2");
}

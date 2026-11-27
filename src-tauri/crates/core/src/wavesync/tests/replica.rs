use super::*;

#[test]
fn a_replica_notices_a_gap_and_ignores_a_resend() {
    let (mut host, me, guest) = session();
    let mut replica = Replica::from_snapshot(host.snapshot()).unwrap();
    let start = replica.seq();
    let mut ops = host
        .handle(
            guest,
            Action::Add {
                entries: vec![entry(1)],
                position: Position::End,
            },
            0,
        )
        .unwrap();
    ops.extend(host.handle(me, Action::Play { entry: None }, 0).unwrap());
    assert_eq!(
        replica.apply(&ops[1]),
        Err(ReplicaError::Gap {
            expected: start + 1,
            got: start + 2
        })
    );
    assert_eq!(replica.seq(), start, "a gap changes nothing");
    assert_eq!(replica.apply(&ops[0]), Ok(Applied::Applied));
    assert_eq!(replica.apply(&ops[0]), Ok(Applied::AlreadyHad));
    assert_eq!(replica.apply(&ops[1]), Ok(Applied::Applied));
    assert_eq!(replica.state(), host.state());
}

#[test]
fn an_op_that_does_not_apply_leaves_the_replica_as_it_was() {
    let (host, _, _) = session();
    let mut replica = Replica::from_snapshot(host.snapshot()).unwrap();
    let before = replica.state().clone();
    let bogus = Sequenced {
        seq: replica.seq() + 1,
        op: Op::EntryRemoved { entry: id(9) },
    };
    assert_eq!(
        replica.apply(&bogus),
        Err(ReplicaError::Diverged(ApplyError::UnknownEntry))
    );
    assert_eq!(replica.state(), &before);
    let bad_cursor = Sequenced {
        seq: replica.seq() + 1,
        op: Op::Transport {
            cursor: 3,
            anchor: Anchor::idle(0),
        },
    };
    assert_eq!(
        replica.apply(&bad_cursor),
        Err(ReplicaError::Diverged(ApplyError::BadCursor))
    );
}

#[test]
fn a_snapshot_is_checked_before_it_is_believed() {
    let (mut host, me, _) = session();
    add(&mut host, me, &[1, 2]);
    host.handle(me, Action::Play { entry: None }, 0).unwrap();
    let good = serde_json::to_value(host.snapshot()).unwrap();

    let mut cursor_past_end = good.clone();
    cursor_past_end["state"]["cursor"] = 7.into();
    let mut anchor_elsewhere = good.clone();
    anchor_elsewhere["state"]["cursor"] = 1.into();
    let mut twin_entries = good.clone();
    let first = twin_entries["state"]["queue"][0].clone();
    twin_entries["state"]["queue"][1] = first;
    let mut twin_people = good.clone();
    let host_row = twin_people["state"]["participants"][0].clone();
    twin_people["state"]["participants"][1] = host_row;

    let mut no_host = good.clone();
    no_host["state"]["participants"][0]["role"] = "guest".into();
    let mut empty_and_past_the_end = good.clone();
    empty_and_past_the_end["state"]["queue"] = serde_json::json!([]);
    empty_and_past_the_end["state"]["anchor"]["entry"] = serde_json::Value::Null;
    empty_and_past_the_end["state"]["anchor"]["playing"] = false.into();
    empty_and_past_the_end["state"]["cursor"] = 1.into();

    // Refused while it is read, so nothing can call an accessor on it first.
    for (bad, why) in [
        (cursor_past_end, ApplyError::BadCursor),
        (anchor_elsewhere, ApplyError::BadCursor),
        (twin_entries, ApplyError::DuplicateEntry),
        (twin_people, ApplyError::DuplicateParticipant),
        (no_host, ApplyError::HostCannotChange),
        (empty_and_past_the_end, ApplyError::BadCursor),
    ] {
        let error = serde_json::from_value::<Snapshot>(bad).unwrap_err();
        assert!(error.to_string().contains(&why.to_string()), "{error}");
    }
    let snapshot: Snapshot = serde_json::from_value(good).unwrap();
    assert_eq!(
        Replica::from_snapshot(snapshot).unwrap().state(),
        host.state()
    );
}

#[test]
fn nothing_applies_after_the_end() {
    let (mut host, me, _) = session();
    let mut replica = Replica::from_snapshot(host.snapshot()).unwrap();
    for op in host.end() {
        replica.apply(&op).unwrap();
    }
    assert!(host.end().is_empty(), "ending twice says nothing more");
    assert!(replica.state().has_ended());
    assert_eq!(host.admit("late").err(), Some(Refusal::SessionEnded));
    assert_eq!(
        host.handle(me, Action::Pause, 0).err(),
        Some(Refusal::SessionEnded)
    );
    let after = Sequenced {
        seq: replica.seq() + 1,
        op: Op::RightsChanged {
            rights: Rights::default(),
        },
    };
    assert_eq!(
        replica.apply(&after),
        Err(ReplicaError::Diverged(ApplyError::Ended))
    );
}

#[test]
fn the_cursor_never_goes_back_and_nothing_moves_without_an_entry() {
    let (mut host, me, _) = session();
    add(&mut host, me, &[1, 2]);
    host.handle(me, Action::Play { entry: None }, 0).unwrap();
    host.handle(me, Action::Next, 10).unwrap();
    let mut replica = Replica::from_snapshot(host.snapshot()).unwrap();
    let back = Sequenced {
        seq: replica.seq() + 1,
        op: Op::Transport {
            cursor: 0,
            anchor: Anchor {
                entry: Some(id(1)),
                position_ms: 0,
                at_ms: 20,
                playing: true,
            },
        },
    };
    assert_eq!(
        replica.apply(&back),
        Err(ReplicaError::Diverged(ApplyError::BadCursor))
    );
    assert!(replica.state().readable(id(1)).is_none(), "still revoked");

    let moving_nothing = Sequenced {
        seq: replica.seq() + 1,
        op: Op::Transport {
            cursor: 1,
            anchor: Anchor {
                entry: None,
                position_ms: 0,
                at_ms: 20,
                playing: true,
            },
        },
    };
    assert_eq!(
        replica.apply(&moving_nothing),
        Err(ReplicaError::Diverged(ApplyError::BadCursor))
    );

    let mut snapshot = serde_json::to_value(host.snapshot()).unwrap();
    snapshot["state"]["anchor"]["entry"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<Snapshot>(snapshot).is_err());

    let second_host = Sequenced {
        seq: replica.seq() + 1,
        op: Op::ParticipantJoined {
            participant: Participant {
                id: ParticipantId(40),
                name: "Usurper".into(),
                role: Role::Host,
                state: ParticipantState::Listening,
            },
        },
    };
    assert_eq!(
        replica.apply(&second_host),
        Err(ReplicaError::Diverged(ApplyError::HostCannotChange))
    );
    let host_leaves = Sequenced {
        seq: replica.seq() + 1,
        op: Op::ParticipantLeft {
            participant: me,
            removed: false,
        },
    };
    assert_eq!(
        replica.apply(&host_leaves),
        Err(ReplicaError::Diverged(ApplyError::HostCannotChange))
    );
}

#[test]
fn an_anchor_past_the_end_of_its_entry_is_refused() {
    let (mut host, me, _) = session();
    add(&mut host, me, &[1]);
    let mut replica = Replica::from_snapshot(host.snapshot()).unwrap();
    let past = Sequenced {
        seq: replica.seq() + 1,
        op: Op::Transport {
            cursor: 0,
            anchor: Anchor {
                entry: Some(id(1)),
                position_ms: 180_001,
                at_ms: 0,
                playing: false,
            },
        },
    };
    assert_eq!(
        replica.apply(&past),
        Err(ReplicaError::Diverged(ApplyError::BadCursor))
    );
    let at_the_end = Sequenced {
        op: Op::Transport {
            cursor: 0,
            anchor: Anchor {
                entry: Some(id(1)),
                position_ms: 180_000,
                at_ms: 0,
                playing: false,
            },
        },
        ..past
    };
    assert_eq!(replica.apply(&at_the_end), Ok(Applied::Applied));
}

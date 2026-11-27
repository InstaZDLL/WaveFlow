use super::*;

#[test]
fn a_session_needs_names_and_cleans_them() {
    assert_eq!(Host::new(" ", "Alex", 0).err(), Some(Refusal::InvalidName));
    assert_eq!(
        Host::new("Friday", "\u{7}", 0).err(),
        Some(Refusal::InvalidName)
    );

    let mut host = Host::new("  Fri\u{0}day  ", "Alex", 0).unwrap();
    assert_eq!(host.state().name(), "Friday");
    let (guest, _) = host.admit(&"x".repeat(100)).unwrap();
    assert_eq!(
        host.state()
            .participant(guest)
            .unwrap()
            .name
            .chars()
            .count(),
        MAX_NAME_CHARS
    );
    assert_eq!(host.admit("\n\t").err(), Some(Refusal::InvalidName));
}

#[test]
fn participants_are_numbered_once_and_capped() {
    let mut host = Host::new("Friday", "Alex", 0).unwrap();
    assert_eq!(host.host_id(), ParticipantId(1));
    let mut admitted = Vec::new();
    for i in 0..MAX_PARTICIPANTS - 1 {
        admitted.push(host.admit(&format!("guest {i}")).unwrap().0);
    }
    assert_eq!(host.admit("one too many").err(), Some(Refusal::SessionFull));

    // A participant who leaves frees a seat, but not their number.
    host.handle(admitted[0], Action::Leave, 0).unwrap();
    let (again, _) = host.admit("late").unwrap();
    assert_eq!(again, ParticipantId(MAX_PARTICIPANTS as u32 + 1));
    assert!(host.state().participant(admitted[0]).is_none());
}

#[test]
fn an_entry_belongs_to_whoever_sent_it() {
    let (mut host, _, guest) = session();
    let mut forged = entry(1);
    forged.owner = host.host_id();
    host.handle(
        guest,
        Action::Add {
            entries: vec![forged],
            position: Position::End,
        },
        0,
    )
    .unwrap();
    assert_eq!(host.state().queue()[0].owner, guest);
}

#[test]
fn a_peers_entry_is_cleaned_or_refused() {
    let (mut host, _, guest) = session();
    let refused = |host: &mut Host, e: Entry| {
        host.handle(
            guest,
            Action::Add {
                entries: vec![e],
                position: Position::End,
            },
            0,
        )
        .err()
    };

    let mut e = entry(1);
    e.title = format!("  A\u{1b}[31mtitle{}  ", "x".repeat(600));
    e.artists = vec!["".into(), "One".into(), "\u{0}".into()];
    e.album = Some("   ".into());
    e.gain = Some(Gain {
        track_gain_db: Some(f32::NAN),
        track_peak: Some(0.9),
        album_gain_db: Some(f32::INFINITY),
        album_peak: None,
    });
    host.handle(
        guest,
        Action::Add {
            entries: vec![e],
            position: Position::End,
        },
        0,
    )
    .unwrap();
    let kept = &host.state().queue()[0];
    assert!(kept.title.starts_with("A[31mtitle"));
    assert_eq!(kept.title.chars().count(), 512);
    assert_eq!(kept.artists, vec!["One".to_string()]);
    assert_eq!(kept.album, None);
    assert_eq!(
        kept.gain,
        Some(Gain {
            track_gain_db: None,
            track_peak: Some(0.9),
            album_gain_db: None,
            album_peak: None,
        })
    );

    let mut dsd = entry(2);
    dsd.codec = "dsf".into();
    assert_eq!(refused(&mut host, dsd), Some(Refusal::NotShareable));

    let mut big = entry(3);
    big.size = MAX_FILE_BYTES + 1;
    assert_eq!(refused(&mut host, big), Some(Refusal::TooLarge));

    for broken in [
        Entry {
            title: " ".into(),
            ..entry(4)
        },
        Entry {
            codec: "FLAC".into(),
            ..entry(4)
        },
        Entry {
            codec: "../x".into(),
            ..entry(4)
        },
        Entry {
            size: 0,
            ..entry(4)
        },
        Entry {
            duration_ms: 0,
            ..entry(4)
        },
        Entry {
            channels: 9,
            ..entry(4)
        },
        Entry {
            sample_rate: 0,
            ..entry(4)
        },
        Entry {
            full_hash: Some("ABC".into()),
            ..entry(4)
        },
    ] {
        assert_eq!(refused(&mut host, broken), Some(Refusal::InvalidEntry));
    }
    // A refused batch adds nothing, not even its valid entries.
    let before = host.state().queue().len();
    let mut bad = entry(6);
    bad.size = 0;
    assert_eq!(
        host.handle(
            guest,
            Action::Add {
                entries: vec![entry(5), bad],
                position: Position::End,
            },
            0,
        )
        .err(),
        Some(Refusal::InvalidEntry)
    );
    assert_eq!(host.state().queue().len(), before);
}

#[test]
fn an_entry_id_is_never_taken_twice_even_after_removal() {
    let (mut host, _, guest) = session();
    add(&mut host, guest, &[1]);
    host.handle(guest, Action::Remove { entry: id(1) }, 0)
        .unwrap();
    let again = host.handle(
        guest,
        Action::Add {
            entries: vec![entry(1)],
            position: Position::End,
        },
        0,
    );
    assert_eq!(again.err(), Some(Refusal::DuplicateEntry));
    let twice = host.handle(
        guest,
        Action::Add {
            entries: vec![entry(2), entry(2)],
            position: Position::End,
        },
        0,
    );
    assert_eq!(twice.err(), Some(Refusal::DuplicateEntry));
}

#[test]
fn the_queue_is_capped_on_what_is_left_to_play() {
    let (mut host, me, _) = session();
    let entries: Vec<Entry> = (0..MAX_UPCOMING as u32)
        .map(|n| {
            let mut bytes = [0u8; 16];
            bytes[..4].copy_from_slice(&n.to_be_bytes());
            Entry {
                id: EntryId::from_bytes(bytes),
                ..entry(0)
            }
        })
        .collect();
    host.handle(
        me,
        Action::Add {
            entries,
            position: Position::End,
        },
        0,
    )
    .unwrap();
    let more = host.handle(
        me,
        Action::Add {
            entries: vec![entry(0xff)],
            position: Position::End,
        },
        0,
    );
    assert_eq!(more.err(), Some(Refusal::QueueFull));
}

#[test]
fn guests_add_at_the_end_and_the_host_anywhere() {
    let (mut host, me, guest) = session();
    add(&mut host, me, &[1, 2]);
    let next = host.handle(
        guest,
        Action::Add {
            entries: vec![entry(3)],
            position: Position::Next,
        },
        0,
    );
    assert_eq!(next.err(), Some(Refusal::NotAllowed));

    host.handle(me, Action::Play { entry: None }, 0).unwrap();
    host.handle(
        me,
        Action::Add {
            entries: vec![entry(3)],
            position: Position::Next,
        },
        0,
    )
    .unwrap();
    host.handle(
        me,
        Action::Add {
            entries: vec![entry(4)],
            position: Position::After(id(3)),
        },
        0,
    )
    .unwrap();
    assert_eq!(ids(host.state().queue()), vec![id(1), id(3), id(4), id(2)]);
}

#[test]
fn only_what_is_left_to_play_is_readable() {
    let (mut host, me, guest) = session();
    add(&mut host, guest, &[1, 2, 3, 4]);
    host.handle(me, Action::Play { entry: None }, 0).unwrap();
    assert!(host.state().readable(id(1)).is_some());
    host.handle(me, Action::Next, 10).unwrap();
    assert!(host.state().readable(id(1)).is_none(), "played");
    assert!(host.state().readable(id(2)).is_some(), "playing");
    assert!(host.state().readable(id(3)).is_some(), "upcoming");
    host.handle(guest, Action::Remove { entry: id(3) }, 20)
        .unwrap();
    assert!(host.state().readable(id(3)).is_none(), "removed");
    assert!(host.state().readable(id(9)).is_none(), "never added");
    // The played entry stays listed, for the panel.
    assert_eq!(host.state().played()[0].id, id(1));

    host.handle(me, Action::EndSession, 30).unwrap();
    assert!(host.state().readable(id(2)).is_none(), "ended");
    assert_eq!(
        host.handle(me, Action::Next, 40).err(),
        Some(Refusal::SessionEnded)
    );
}

#[test]
fn playing_skips_the_entries_of_whoever_is_away() {
    let (mut host, me, guest) = session();
    add(&mut host, guest, &[1, 2]);
    add(&mut host, me, &[3]);
    host.connection_lost(guest);
    host.handle(me, Action::Play { entry: None }, 100).unwrap();
    let state = host.state();
    assert_eq!(state.current().map(|e| e.id), Some(id(3)));
    assert_eq!(state.cursor(), 2, "the skipped entries count as played");
    assert!(state.readable(id(1)).is_none());
    assert_eq!(
        host.handle(me, Action::Play { entry: Some(id(1)) }, 100)
            .err(),
        Some(Refusal::EntryGone)
    );

    // Back before their turn comes: their next entry plays.
    let (mut host, me, guest) = session();
    add(&mut host, me, &[1]);
    add(&mut host, guest, &[2]);
    host.handle(me, Action::Play { entry: None }, 0).unwrap();
    host.connection_lost(guest);
    host.reconnected(guest);
    host.handle(me, Action::Ended { entry: id(1) }, 180_000)
        .unwrap();
    assert_eq!(host.state().current().map(|e| e.id), Some(id(2)));

    // An away guest cannot add.
    host.connection_lost(guest);
    let away = host.handle(
        guest,
        Action::Add {
            entries: vec![entry(7)],
            position: Position::End,
        },
        0,
    );
    assert_eq!(away.err(), Some(Refusal::NotAllowed));
    assert_eq!(
        host.handle(
            guest,
            Action::SetState {
                state: ParticipantState::Away
            },
            0
        )
        .err(),
        Some(Refusal::NotAllowed),
        "only the host marks someone away"
    );
}

#[test]
fn nothing_to_play_is_said_and_the_end_of_the_queue_stops() {
    let (mut host, me, _) = session();
    assert_eq!(
        host.handle(me, Action::Play { entry: None }, 0).err(),
        Some(Refusal::NothingToPlay)
    );
    add(&mut host, me, &[1]);
    host.handle(me, Action::Play { entry: None }, 0).unwrap();
    host.handle(me, Action::Next, 5).unwrap();
    let state = host.state();
    assert_eq!(state.current(), None);
    assert_eq!(state.cursor(), 1);
    assert_eq!(state.anchor(), Anchor::idle(5));
}

#[test]
fn a_late_end_report_does_not_skip_twice() {
    let (mut host, me, _) = session();
    add(&mut host, me, &[1, 2, 3]);
    host.handle(me, Action::Play { entry: None }, 0).unwrap();
    host.handle(me, Action::Next, 1_000).unwrap();
    // The player reports the end of 1 after the user already skipped it.
    let ops = host
        .handle(me, Action::Ended { entry: id(1) }, 1_200)
        .unwrap();
    assert!(ops.is_empty());
    assert_eq!(host.state().current().map(|e| e.id), Some(id(2)));
    host.handle(me, Action::Ended { entry: id(2) }, 181_000)
        .unwrap();
    assert_eq!(host.state().current().map(|e| e.id), Some(id(3)));
}

#[test]
fn removing_what_plays_skips_it_for_everyone() {
    let (mut host, me, guest) = session();
    add(&mut host, guest, &[1]);
    add(&mut host, me, &[2]);
    host.handle(me, Action::Play { entry: None }, 0).unwrap();
    assert_eq!(
        host.handle(guest, Action::Remove { entry: id(2) }, 0).err(),
        Some(Refusal::NotAllowed),
        "a guest removes only their own"
    );
    let ops = host
        .handle(guest, Action::Remove { entry: id(1) }, 50)
        .unwrap();
    assert_eq!(ops.len(), 2);
    let state = host.state();
    assert_eq!(state.current().map(|e| e.id), Some(id(2)));
    assert!(state.anchor().playing);
    assert_eq!(state.anchor().at_ms, 50);
    assert_eq!(
        host.handle(guest, Action::Remove { entry: id(1) }, 0).err(),
        Some(Refusal::EntryGone)
    );
}

#[test]
fn the_transport_is_the_hosts_unless_the_session_shares_it() {
    let (mut host, me, guest) = session();
    add(&mut host, me, &[1, 2]);
    host.handle(me, Action::Play { entry: None }, 0).unwrap();
    for action in [
        Action::Pause,
        Action::Next,
        Action::Previous,
        Action::Seek { position_ms: 5 },
        Action::Play { entry: Some(id(2)) },
    ] {
        assert_eq!(
            host.handle(guest, action.clone(), 0).err(),
            Some(Refusal::NotAllowed),
            "{action:?}"
        );
    }
    for action in [
        Action::Move {
            entry: id(2),
            position: Position::End,
        },
        Action::SetRights {
            rights: Rights::default(),
        },
        Action::EndSession,
        Action::Ended { entry: id(1) },
        Action::RemoveParticipant { participant: me },
    ] {
        assert_eq!(
            host.handle(guest, action.clone(), 0).err(),
            Some(Refusal::NotAllowed),
            "{action:?}"
        );
    }

    host.handle(
        me,
        Action::SetRights {
            rights: Rights {
                everyone_controls_playback: true,
            },
        },
        0,
    )
    .unwrap();
    host.handle(guest, Action::Pause, 10_000).unwrap();
    let anchor = host.state().anchor();
    assert!(!anchor.playing);
    assert_eq!(anchor.position_ms, 10_000);
    host.handle(
        guest,
        Action::Seek {
            position_ms: 999_999,
        },
        11_000,
    )
    .unwrap();
    assert_eq!(host.state().anchor().position_ms, 180_000, "clamped");
    host.handle(guest, Action::Play { entry: None }, 12_000)
        .unwrap();
    let anchor = host.state().anchor();
    assert!(anchor.playing);
    assert_eq!((anchor.position_ms, anchor.at_ms), (180_000, 12_000));
    host.handle(guest, Action::Previous, 13_000).unwrap();
    assert_eq!(host.state().anchor().position_ms, 0);
}

#[test]
fn a_pause_lands_where_the_host_was() {
    let (mut host, me, _) = session();
    add(&mut host, me, &[1]);
    host.handle(me, Action::Play { entry: None }, 1_000)
        .unwrap();
    host.handle(me, Action::Pause, 4_500).unwrap();
    let anchor = host.state().anchor();
    assert_eq!(
        (anchor.position_ms, anchor.at_ms, anchor.playing),
        (3_500, 4_500, false)
    );
    assert_eq!(anchor.position_at(99_999), 3_500);
    // Pausing again changes nothing and makes no op.
    assert!(host.handle(me, Action::Pause, 5_000).unwrap().is_empty());
}

#[test]
fn the_host_moves_what_has_not_played() {
    let (mut host, me, _) = session();
    add(&mut host, me, &[1, 2, 3, 4]);
    host.handle(me, Action::Play { entry: None }, 0).unwrap();
    host.handle(
        me,
        Action::Move {
            entry: id(4),
            position: Position::Next,
        },
        0,
    )
    .unwrap();
    assert_eq!(ids(host.state().queue()), vec![id(1), id(4), id(2), id(3)]);
    host.handle(
        me,
        Action::Move {
            entry: id(4),
            position: Position::After(id(3)),
        },
        0,
    )
    .unwrap();
    assert_eq!(ids(host.state().queue()), vec![id(1), id(2), id(3), id(4)]);
    for refused in [
        Action::Move {
            entry: id(1),
            position: Position::End,
        },
        Action::Move {
            entry: id(2),
            position: Position::After(id(2)),
        },
    ] {
        assert_eq!(
            host.handle(me, refused.clone(), 0).err(),
            Some(Refusal::NotAllowed),
            "{refused:?}"
        );
    }
}

#[test]
fn a_guest_who_leaves_takes_what_they_had_left_to_play() {
    let (mut host, me, guest) = session();
    add(&mut host, guest, &[1, 2]);
    add(&mut host, me, &[3]);
    add(&mut host, guest, &[4]);
    host.handle(me, Action::Play { entry: None }, 0).unwrap();
    host.handle(guest, Action::Leave, 10).unwrap();
    let state = host.state();
    // The one playing finishes; the others go.
    assert_eq!(ids(state.queue()), vec![id(1), id(3)]);
    assert_eq!(state.current().map(|e| e.id), Some(id(1)));
    assert!(state.participant(guest).is_none());
    assert_eq!(
        host.handle(guest, Action::Next, 0).err(),
        Some(Refusal::UnknownParticipant)
    );
    host.handle(me, Action::Ended { entry: id(1) }, 180_000)
        .unwrap();
    assert_eq!(host.state().current().map(|e| e.id), Some(id(3)));
}

#[test]
fn the_host_removes_a_guest_but_never_itself() {
    let (mut host, me, guest) = session();
    let (other, _) = host.admit("Kim").unwrap();
    assert_eq!(
        host.handle(guest, Action::RemoveParticipant { participant: other }, 0)
            .err(),
        Some(Refusal::NotAllowed),
        "a guest removes nobody"
    );
    assert_eq!(
        host.handle(me, Action::RemoveParticipant { participant: me }, 0)
            .err(),
        Some(Refusal::NotAllowed)
    );
    assert_eq!(
        host.handle(me, Action::Leave, 0).err(),
        Some(Refusal::NotAllowed)
    );
    let ops = host
        .handle(me, Action::RemoveParticipant { participant: guest }, 0)
        .unwrap();
    assert_eq!(
        ops.last().map(|s| &s.op),
        Some(&Op::ParticipantLeft {
            participant: guest,
            removed: true
        })
    );
    assert_eq!(
        host.handle(me, Action::RemoveParticipant { participant: guest }, 0)
            .err(),
        Some(Refusal::UnknownParticipant)
    );
}

#[test]
fn a_reconnection_gets_what_it_missed_or_a_snapshot() {
    let (mut host, me, guest) = session();
    let at = host.seq();
    assert_eq!(host.catch_up(at), CatchUp::UpToDate);
    add(&mut host, guest, &[1]);
    add(&mut host, guest, &[2]);
    match host.catch_up(at) {
        CatchUp::Ops(ops) => assert_eq!(
            ops.iter().map(|s| s.seq).collect::<Vec<_>>(),
            vec![at + 1, at + 2]
        ),
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(host.catch_up(at + 99), CatchUp::Snapshot(_)),
        "from the future"
    );

    // Run past what the log keeps: the first ops are gone.
    for n in 0..LOG_KEPT as u64 {
        host.handle(me, Action::Seek { position_ms: n % 2 }, 0).ok();
        host.handle(
            me,
            Action::SetRights {
                rights: Rights {
                    everyone_controls_playback: n % 2 == 0,
                },
            },
            0,
        )
        .unwrap();
    }
    match host.catch_up(at) {
        CatchUp::Snapshot(snapshot) => {
            assert_eq!(snapshot.seq, host.seq());
            assert_eq!(&snapshot.state, host.state());
        }
        other => panic!("{other:?}"),
    }
    let oldest_kept = host.seq() - LOG_KEPT as u64 + 1;
    assert!(matches!(host.catch_up(oldest_kept - 1), CatchUp::Ops(ops) if ops.len() == LOG_KEPT));
    assert!(matches!(
        host.catch_up(oldest_kept - 2),
        CatchUp::Snapshot(_)
    ));
}

#[test]
fn played_entries_are_trimmed_the_same_way_everywhere() {
    let (mut host, me, _) = session();
    let mut replica = Replica::from_snapshot(host.snapshot()).unwrap();
    let total = MAX_PLAYED_KEPT + 20;
    let entries: Vec<Entry> = (0..total as u32)
        .map(|n| {
            let mut bytes = [1u8; 16];
            bytes[..4].copy_from_slice(&n.to_be_bytes());
            Entry {
                id: EntryId::from_bytes(bytes),
                ..entry(0)
            }
        })
        .collect();
    let last = entries.last().unwrap().id;
    let mut ops = host
        .handle(
            me,
            Action::Add {
                entries,
                position: Position::End,
            },
            0,
        )
        .unwrap();
    ops.extend(host.handle(me, Action::Play { entry: None }, 0).unwrap());
    for t in 1..total as u64 {
        ops.extend(host.handle(me, Action::Next, t).unwrap());
    }
    for op in &ops {
        replica.apply(op).unwrap();
    }
    let state = host.state();
    assert_eq!(state.current().map(|e| e.id), Some(last));
    assert_eq!(state.cursor(), MAX_PLAYED_KEPT);
    assert_eq!(state.queue().len(), MAX_PLAYED_KEPT + 1);
    assert_eq!(replica.state(), state);
}

/// A whole evening of requests from three people, valid and not, in a fixed
/// pseudo-random order: a replica fed every op the host made ends where the
/// host is, at every step, and a replica that joined halfway from a snapshot
/// does too.
#[test]
fn a_replica_follows_the_host_through_an_evening() {
    let mut host = Host::new("Friday", "Alex", 0).unwrap();
    let me = host.host_id();
    let mut replica = Replica::from_snapshot(host.snapshot()).unwrap();
    let mut late: Option<Replica> = None;
    let mut people = vec![me];
    let feed = |ops: Vec<Sequenced>, replica: &mut Replica, late: &mut Option<Replica>| {
        for op in ops {
            assert_eq!(replica.apply(&op), Ok(Applied::Applied));
            if let Some(late) = late.as_mut() {
                assert_eq!(late.apply(&op), Ok(Applied::Applied));
            }
        }
    };
    for name in ["Sam", "Kim"] {
        let (p, ops) = host.admit(name).unwrap();
        people.push(p);
        feed(ops, &mut replica, &mut late);
    }

    let mut seed: u64 = 0x5eed;
    let mut next = move || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (seed >> 33) as u32
    };
    let mut minted: u8 = 0;
    for step in 0..2_000u64 {
        let who = people[next() as usize % people.len()];
        let pick = |n: u32, host: &Host| {
            let q = host.state().queue();
            (!q.is_empty()).then(|| q[n as usize % q.len()].id)
        };
        let now = step * 1_000;
        let ops = match next() % 12 {
            0 | 1 => {
                minted = minted.wrapping_add(1);
                host.handle(
                    who,
                    Action::Add {
                        entries: vec![entry(minted)],
                        position: if next() % 2 == 0 {
                            Position::End
                        } else {
                            Position::Next
                        },
                    },
                    now,
                )
            }
            2 => match pick(next(), &host) {
                Some(entry) => host.handle(who, Action::Remove { entry }, now),
                None => Ok(Vec::new()),
            },
            3 => match (pick(next(), &host), pick(next(), &host)) {
                (Some(entry), Some(after)) => host.handle(
                    who,
                    Action::Move {
                        entry,
                        position: Position::After(after),
                    },
                    now,
                ),
                _ => Ok(Vec::new()),
            },
            4 => host.handle(
                who,
                Action::Play {
                    entry: pick(next(), &host),
                },
                now,
            ),
            5 => host.handle(who, Action::Pause, now),
            6 => host.handle(who, Action::Next, now),
            7 => host.handle(
                who,
                Action::Seek {
                    position_ms: u64::from(next() % 200_000),
                },
                now,
            ),
            8 => match host.state().anchor().entry {
                Some(entry) => host.handle(me, Action::Ended { entry }, now),
                None => Ok(Vec::new()),
            },
            9 => host.handle(
                me,
                Action::SetRights {
                    rights: Rights {
                        everyone_controls_playback: next() % 2 == 0,
                    },
                },
                now,
            ),
            10 => {
                let guest = people[1 + next() as usize % (people.len() - 1)];
                Ok(if next() % 2 == 0 {
                    host.connection_lost(guest)
                } else {
                    host.reconnected(guest)
                })
            }
            _ => host.handle(
                who,
                Action::SetState {
                    state: if next() % 2 == 0 {
                        ParticipantState::Buffering
                    } else {
                        ParticipantState::Listening
                    },
                },
                now,
            ),
        };
        if let Ok(ops) = ops {
            feed(ops, &mut replica, &mut late);
        }
        assert_eq!(replica.state(), host.state(), "step {step}");
        assert_eq!(replica.seq(), host.seq());
        host.state().check().unwrap();
        if step == 1_000 {
            late = Some(Replica::from_snapshot(host.snapshot()).unwrap());
        }
        if let Some(late) = &late {
            assert_eq!(late.state(), host.state(), "late, step {step}");
        }
    }
    assert!(host.seq() > 500, "the evening did things: {}", host.seq());
}

#[test]
fn a_pause_after_the_end_stops_on_the_last_millisecond() {
    let (mut host, me, _) = session();
    add(&mut host, me, &[1]);
    host.handle(me, Action::Play { entry: None }, 0).unwrap();
    // The end has not been reported yet.
    host.handle(me, Action::Pause, 181_000).unwrap();
    assert_eq!(host.state().anchor().position_ms, 180_000);
}

#[test]
fn only_the_host_brings_someone_back_from_away() {
    let (mut host, _, guest) = session();
    host.connection_lost(guest);
    assert_eq!(
        host.handle(
            guest,
            Action::SetState {
                state: ParticipantState::Listening
            },
            0
        )
        .err(),
        Some(Refusal::NotAllowed)
    );
    assert_eq!(
        host.state().participant(guest).unwrap().state,
        ParticipantState::Away
    );
    host.reconnected(guest);
    host.handle(
        guest,
        Action::SetState {
            state: ParticipantState::Buffering,
        },
        0,
    )
    .unwrap();
    assert_eq!(
        host.state().participant(guest).unwrap().state,
        ParticipantState::Buffering
    );
}

#[test]
fn a_paused_entry_whose_owner_left_is_not_resumed() {
    let (mut host, me, guest) = session();
    add(&mut host, guest, &[1]);
    add(&mut host, me, &[2]);
    host.handle(me, Action::Play { entry: None }, 0).unwrap();
    host.handle(me, Action::Pause, 1_000).unwrap();
    host.connection_lost(guest);
    host.handle(me, Action::Play { entry: None }, 2_000)
        .unwrap();
    let state = host.state();
    assert_eq!(state.current().map(|e| e.id), Some(id(2)));
    assert!(state.anchor().playing);

    // Nothing after it: resuming says so instead of playing what cannot play.
    let (mut host, me, guest) = session();
    add(&mut host, guest, &[1]);
    host.handle(me, Action::Play { entry: None }, 0).unwrap();
    host.handle(me, Action::Pause, 1_000).unwrap();
    host.handle(guest, Action::Leave, 1_500).unwrap();
    assert_eq!(
        host.handle(me, Action::Play { entry: None }, 2_000).err(),
        Some(Refusal::NothingToPlay)
    );
}

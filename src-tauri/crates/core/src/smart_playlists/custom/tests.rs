use super::*;

// SQL-shape assertions live in a nested module gated on `sqlite`
// because `build_node_sql` (and its `BindValue` argument) are
// sqlite-only. The serde / migration tests below stay outside this
// module so a postgres-only test run still exercises the v1 → v2
// rule-tree migration contract.
#[cfg(feature = "sqlite")]
mod sql_tests {
    use super::*;

    /// A pinned clock, so the relative windows below assert on a
    /// value rather than on "whatever the test machine thinks the
    /// time is". 2026-01-01T00:00:00Z.
    const NOW: i64 = 1_767_225_600_000;

    /// Helper: build SQL string from a tree (binds are discarded for
    /// readability — the tests only assert on the textual shape).
    fn sql_of(node: &RuleNode) -> String {
        let mut binds = Vec::new();
        build_node_sql(node, &mut binds, NOW)
    }

    /// Helper: the binds a tree emits, in placeholder order.
    fn binds_of(node: &RuleNode) -> Vec<BindValue> {
        let mut binds = Vec::new();
        build_node_sql(node, &mut binds, NOW);
        binds
    }

    fn leaf(predicate: Predicate) -> RuleNode {
        RuleNode::Leaf { predicate }
    }

    #[test]
    fn empty_all_matches_everything() {
        assert_eq!(sql_of(&RuleNode::All { children: vec![] }), "1=1");
    }

    #[test]
    fn empty_any_matches_nothing() {
        assert_eq!(sql_of(&RuleNode::Any { children: vec![] }), "0=1");
    }

    #[test]
    fn single_leaf_wraps_predicate() {
        let n = RuleNode::Leaf {
            predicate: Predicate::TitleContains {
                value: "foo".into(),
            },
        };
        assert_eq!(sql_of(&n), "t.title LIKE ? ESCAPE '!'");
    }

    /// A `LIKE` metacharacter typed into a "contains" field is a
    /// character, not a wildcard — and the escape character itself
    /// has to survive being typed.
    #[test]
    fn a_contains_pattern_escapes_its_metacharacters() {
        assert_eq!(like_contains("My_Music"), "%My!_Music%");
        assert_eq!(like_contains("100%"), "%100!%%");
        assert_eq!(like_contains("Hey!"), "%Hey!!%");
    }

    #[test]
    fn and_joins_with_and() {
        let n = RuleNode::All {
            children: vec![
                RuleNode::Leaf {
                    predicate: Predicate::HiRes,
                },
                RuleNode::Leaf {
                    predicate: Predicate::Liked,
                },
            ],
        };
        let s = sql_of(&n);
        assert!(s.contains(" AND "));
        assert!(s.starts_with('(') && s.ends_with(')'));
    }

    #[test]
    fn or_joins_with_or() {
        let n = RuleNode::Any {
            children: vec![
                RuleNode::Leaf {
                    predicate: Predicate::GenreIs { value: 1 },
                },
                RuleNode::Leaf {
                    predicate: Predicate::GenreIs { value: 2 },
                },
            ],
        };
        let s = sql_of(&n);
        assert!(s.contains(" OR "));
    }

    #[test]
    fn not_wraps_child() {
        let n = RuleNode::Not {
            child: Box::new(RuleNode::Leaf {
                predicate: Predicate::Liked,
            }),
        };
        let s = sql_of(&n);
        assert!(s.starts_with("NOT ("));
        assert!(s.ends_with(')'));
    }

    /// A relative window is turned into an absolute cut-off at
    /// build time. This pins the arithmetic — the unit is
    /// milliseconds, and getting it wrong by a factor of 1 000
    /// would silently widen "the last 30 days" to eighty years.
    #[test]
    fn a_window_binds_a_cut_off_in_milliseconds() {
        let binds = binds_of(&leaf(Predicate::PlayedInLastDays { value: 30 }));
        assert_eq!(binds.len(), 1);
        let BindValue::Int(cut_off) = binds[0] else {
            panic!("expected an integer bind");
        };
        assert_eq!(cut_off, NOW - 30 * 86_400_000);
    }

    /// An absurd window must clamp, not wrap. A negative cut-off
    /// still matches every play ever recorded; a wrapped one would
    /// land in the future and match none, which is the opposite of
    /// what "the last four billion days" asks for.
    #[test]
    fn an_absurd_window_clamps_instead_of_wrapping() {
        assert!(
            days_ago_ms(NOW, i64::MAX) < 0,
            "an absurd window must stay in the past"
        );
        assert_eq!(days_ago_ms(NOW, -5), NOW, "a negative window is no window");
    }

    /// Bounds on a nullable column carry their own NULL guard, so
    /// that a track missing the value falls out of the rule *and*
    /// into its negation rather than out of both.
    #[test]
    fn nullable_bounds_guard_their_column() {
        for p in [
            Predicate::SampleRateMin { value: 88_200 },
            Predicate::BitDepthMin { value: 24 },
            Predicate::DiscNumberIs { value: 2 },
            Predicate::HiRes,
        ] {
            let s = sql_of(&leaf(p.clone()));
            assert!(s.contains("IS NOT NULL"), "{p:?} must guard its NULLs: {s}");
        }
    }

    /// The tag name is a bind like any other value. It is the one
    /// place where user input names a *column's content* rather
    /// than being compared to one, so it is also the one place
    /// where an interpolated build would be tempting.
    #[test]
    fn a_tag_rule_binds_its_key() {
        let s = sql_of(&leaf(Predicate::TagContains {
            key: "COMPOSER".into(),
            value: "Glass".into(),
        }));
        assert!(
            !s.contains("COMPOSER"),
            "the key must not reach the SQL: {s}"
        );
        assert_eq!(
            s.matches('?').count(),
            2,
            "one bind for the key, one for the value"
        );
    }

    /// Both sides of a path rule are folded to `/`, so the same
    /// rule reads the same whichever separator is on either end.
    #[test]
    fn a_path_rule_folds_both_separators() {
        let binds = binds_of(&leaf(Predicate::PathContains {
            value: r"Live\Bootlegs".into(),
        }));
        let BindValue::Text(needle) = &binds[0] else {
            panic!("expected a text bind");
        };
        assert_eq!(needle, "%Live/Bootlegs%");
        assert!(sql_of(&leaf(Predicate::PathContains { value: "x".into() })).contains("REPLACE"));
    }

    /// Enough of the profile schema for the rule queries to run.
    ///
    /// Hand-rolled rather than the real migrations, which live in
    /// the app crate and are out of reach from `waveflow-core` —
    /// the same constraint the scanner and repository fixtures
    /// note. Only the columns the predicates read are here; the
    /// collation on `track_tag.key` is NOT optional decoration, it
    /// is what a tag rule depends on to match a key the scanner
    /// stored upper-cased.
    async fn fixture_pool(now_ms: i64) -> sqlx::SqlitePool {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE track (
                     id           INTEGER PRIMARY KEY,
                     title        TEXT NOT NULL,
                     file_path    TEXT NOT NULL,
                     duration_ms  INTEGER NOT NULL,
                     added_at     INTEGER NOT NULL,
                     is_available INTEGER NOT NULL DEFAULT 1,
                     sample_rate  INTEGER,
                     bit_depth    INTEGER,
                     disc_number  INTEGER,
                     year         INTEGER,
                     rating       INTEGER,
                     codec        TEXT,
                     album_id     INTEGER,
                     primary_artist INTEGER
                 );
                 CREATE TABLE play_event (
                     id        INTEGER PRIMARY KEY,
                     track_id  INTEGER,
                     played_at INTEGER NOT NULL
                 );
                 CREATE TABLE track_tag (
                     track_id INTEGER NOT NULL,
                     key      TEXT NOT NULL COLLATE NOCASE,
                     value    TEXT NOT NULL,
                     PRIMARY KEY (track_id, key)
                 );",
        )
        .execute(&pool)
        .await
        .unwrap();

        let day = 86_400_000_i64;
        // 1 — a Windows path, hi-res, disc 2, played five days ago,
        //     and carrying a composer tag written in upper case.
        // 2 — a POSIX path, lossy, no bit depth, last played more
        //     than a year ago.
        // 3 — matches nearly everything, but the file is gone.
        sqlx::query(
            "INSERT INTO track
                   (id, title, file_path, duration_ms, added_at, is_available,
                    sample_rate, bit_depth, disc_number)
                 VALUES
                   (1, 'Alpha', ?, 200000, ?, 1, 96000, 24, 2),
                   (2, 'Beta', '/home/u/Music/Studio/b.mp3', 200000, ?, 1,
                    44100, NULL, NULL),
                   (3, 'Gamma', '/home/u/Music/Live/Bootlegs/c.flac', 200000,
                    ?, 0, 96000, 24, 2)",
        )
        .bind(r"E:\Music\Live\Bootlegs\a.flac")
        .bind(now_ms - 10 * day)
        .bind(now_ms - 100 * day)
        .bind(now_ms - day)
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO play_event (track_id, played_at) VALUES
                   (1, ?), (1, ?), (2, ?)",
        )
        .bind(now_ms - 5 * day)
        .bind(now_ms - 400 * day)
        .bind(now_ms - 400 * day)
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO track_tag (track_id, key, value) VALUES (1, 'COMPOSER', 'Philip Glass')",
        )
        .execute(&pool)
        .await
        .unwrap();

        pool
    }

    fn rules(tree: RuleNode) -> CustomRules {
        CustomRules {
            tree,
            sort: None,
            limit: None,
        }
    }

    async fn matched(pool: &sqlx::SqlitePool, tree: RuleNode) -> Vec<i64> {
        run_query_at(pool, &rules(tree), NOW).await.unwrap()
    }

    /// A fake lossless file is a lossless format with a wall: the same
    /// cut in an MP3 is just what an MP3 is, and a lossless file the
    /// measure found no wall in is genuine.
    #[tokio::test]
    async fn fake_lossless_needs_a_lossless_file_and_a_wall() {
        let pool = fixture_pool(NOW).await;
        sqlx::raw_sql(
            "CREATE TABLE track_analysis (track_id INTEGER PRIMARY KEY, cutoff_hz REAL);
                 UPDATE track SET codec = 'FLAC' WHERE id IN (1, 3);
                 UPDATE track SET codec = 'MP3' WHERE id = 2;
                 INSERT INTO track_analysis VALUES (1, 16000), (2, 16000), (3, 16000);",
        )
        .execute(&pool)
        .await
        .unwrap();
        // 2 is an MP3, 3 is gone from disk.
        assert_eq!(matched(&pool, leaf(Predicate::FakeLossless)).await, vec![1]);

        sqlx::query("UPDATE track_analysis SET cutoff_hz = NULL WHERE track_id = 1")
            .execute(&pool)
            .await
            .unwrap();
        assert!(matched(&pool, leaf(Predicate::FakeLossless))
            .await
            .is_empty());

        // A 32 kHz file: its converter's filter stands just under
        // Nyquist (15.5 kHz here), under the fixed threshold but not a
        // lossy encoder's cut.
        sqlx::raw_sql(
            "UPDATE track SET sample_rate = 32000 WHERE id = 1;
                 UPDATE track_analysis SET cutoff_hz = 15500 WHERE track_id = 1;",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(matched(&pool, leaf(Predicate::FakeLossless))
            .await
            .is_empty());
        // The verdict is a boolean even for a file of unknown format,
        // which the Properties dialog selects as one.
        sqlx::query("UPDATE track SET codec = NULL WHERE id = 2")
            .execute(&pool)
            .await
            .unwrap();
        let fake: Option<bool> = sqlx::query_scalar(concat!(
            "SELECT ",
            crate::probable_fake_lossless_sql!(),
            " FROM track t, track_analysis ta WHERE t.id = 2 AND ta.track_id = 2"
        ))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(fake, Some(false));

        // At 44.1 kHz the same wall is a lossy encoder's.
        sqlx::query("UPDATE track SET sample_rate = 44100 WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(matched(&pool, leaf(Predicate::FakeLossless)).await, vec![1]);
    }

    /// The new predicates, against a real SQLite.
    ///
    /// The shape tests above assert on strings, which cannot tell a
    /// valid fragment from one SQLite refuses — and `REPLACE(…,
    /// '\\', '/')` is exactly the kind of fragment that either
    /// parses or does not. This one executes them.
    #[tokio::test]
    async fn the_new_predicates_run_and_select_what_they_claim() {
        let pool = fixture_pool(NOW).await;

        // A rule typed with forward slashes finds the file whose
        // path was written by Windows, and vice versa.
        assert_eq!(
            matched(
                &pool,
                leaf(Predicate::PathContains {
                    value: "Live/Bootlegs".into()
                })
            )
            .await,
            vec![1],
            "the separator fold must work in both directions, and the \
             unavailable track must stay out"
        );

        // Case-insensitivity comes from `LIKE` itself, not from a
        // collation: SQLite's `LIKE` ignores collating sequences.
        // This is the assertion that says so out loud, now that no
        // `COLLATE` sits in the fragment to suggest otherwise.
        assert_eq!(
            matched(
                &pool,
                leaf(Predicate::TitleContains {
                    value: "alpha".into()
                })
            )
            .await,
            vec![1],
            "a lower-case needle must find an upper-case title"
        );

        assert_eq!(
            matched(&pool, leaf(Predicate::PlayCountMin { value: 2 })).await,
            vec![1]
        );
        assert_eq!(
            matched(&pool, leaf(Predicate::PlayCountMax { value: 1 })).await,
            vec![2]
        );
        assert_eq!(
            matched(&pool, leaf(Predicate::PlayedInLastDays { value: 30 })).await,
            vec![1]
        );
        assert_eq!(
            matched(&pool, leaf(Predicate::AddedInLastDays { value: 30 })).await,
            vec![1]
        );
        assert_eq!(
            matched(&pool, leaf(Predicate::SampleRateMin { value: 88_200 })).await,
            vec![1]
        );
        assert_eq!(
            matched(&pool, leaf(Predicate::DiscNumberIs { value: 2 })).await,
            vec![1]
        );

        // The NULL guard, end to end: track 2 has no bit depth, so
        // it falls out of the bound — and INTO its negation. Left
        // to SQLite's three-valued logic it would fall out of both.
        assert_eq!(
            matched(&pool, leaf(Predicate::BitDepthMin { value: 24 })).await,
            vec![1]
        );
        assert_eq!(
            matched(
                &pool,
                RuleNode::Not {
                    child: Box::new(leaf(Predicate::BitDepthMin { value: 24 }))
                }
            )
            .await,
            vec![2],
            "a track with no bit depth belongs to the negation"
        );
        // The same question asked of `hi_res`, which is two bounds
        // at once: track 2 has a sample rate and no bit depth, and
        // is hi-res by neither.
        assert_eq!(
            matched(
                &pool,
                RuleNode::Not {
                    child: Box::new(leaf(Predicate::HiRes))
                }
            )
            .await,
            vec![2],
            "a track that is hi-res by neither figure belongs to the negation"
        );

        // The key is stored upper-cased and asked for in lower —
        // `COLLATE NOCASE` on the column is what makes that work.
        assert_eq!(
            matched(
                &pool,
                leaf(Predicate::TagPresent {
                    key: "composer".into()
                })
            )
            .await,
            vec![1]
        );
        assert_eq!(
            matched(
                &pool,
                leaf(Predicate::TagContains {
                    key: "COMPOSER".into(),
                    value: "glass".into(),
                })
            )
            .await,
            vec![1]
        );
        assert!(matched(
            &pool,
            leaf(Predicate::TagContains {
                key: "COMPOSER".into(),
                value: "Reich".into(),
            })
        )
        .await
        .is_empty());

        // An underscore is a character in a path, not a wildcard —
        // and this is the assertion that proves the `ESCAPE` clause
        // reached the SQL, since a pattern escaped without one
        // matches the escape character literally.
        assert!(
            matched(
                &pool,
                leaf(Predicate::PathContains {
                    value: "M_sic".into()
                })
            )
            .await
            .is_empty(),
            "an underscore must not match an arbitrary character"
        );
        assert_eq!(
            matched(
                &pool,
                leaf(Predicate::PathContains {
                    value: "Music".into()
                })
            )
            .await,
            vec![1, 2],
            "and the same rule without it still matches"
        );
    }

    /// "Forgotten" — played once, but not lately. The template of
    /// the same name is this tree, and it is the one shape where a
    /// count and a window have to agree: without the count it would
    /// also sweep in everything never played at all.
    #[tokio::test]
    async fn forgotten_is_played_once_but_not_lately() {
        let pool = fixture_pool(NOW).await;
        let tree = RuleNode::All {
            children: vec![
                leaf(Predicate::PlayCountMin { value: 1 }),
                RuleNode::Not {
                    child: Box::new(leaf(Predicate::PlayedInLastDays { value: 180 })),
                },
            ],
        };
        assert_eq!(matched(&pool, tree).await, vec![2]);
    }

    /// The counter reports the matches *above* the limit, which is
    /// the number a truncated list cannot give and the reason the
    /// editor shows both.
    #[tokio::test]
    async fn the_count_separates_what_matches_from_what_is_kept() {
        let pool = fixture_pool(NOW).await;
        let mut r = rules(RuleNode::All { children: vec![] });
        let all = count_matches_at(&pool, &r, NOW).await.unwrap();
        assert_eq!(
            all,
            RulesCount { total: 2, kept: 2 },
            "the unavailable track counts for neither"
        );

        r.limit = Some(1);
        assert_eq!(
            count_matches_at(&pool, &r, NOW).await.unwrap(),
            RulesCount { total: 2, kept: 1 }
        );
        assert_eq!(
            run_query_at(&pool, &r, NOW).await.unwrap().len(),
            1,
            "`kept` must be what the query actually returns"
        );
    }

    #[test]
    fn nested_tree_renders_full_expression() {
        // (artist=X OR artist=Y) AND year >= 2000 AND NOT liked
        let n = RuleNode::All {
            children: vec![
                RuleNode::Any {
                    children: vec![
                        RuleNode::Leaf {
                            predicate: Predicate::ArtistContains { value: "X".into() },
                        },
                        RuleNode::Leaf {
                            predicate: Predicate::ArtistContains { value: "Y".into() },
                        },
                    ],
                },
                RuleNode::Leaf {
                    predicate: Predicate::YearMin { value: 2000 },
                },
                RuleNode::Not {
                    child: Box::new(RuleNode::Leaf {
                        predicate: Predicate::Liked,
                    }),
                },
            ],
        };
        let s = sql_of(&n);
        assert!(s.contains(" OR "));
        assert!(s.contains(" AND "));
        assert!(s.contains("NOT ("));
    }
}

#[test]
fn migrate_v1_flat_rules_to_tree() {
    // Old-shape JSON — what's already sitting in user DBs.
    let v1 = r#"{
            "title_contains": "foo",
            "year_min": 2020,
            "genre_ids": [1, 2, 3],
            "liked_only": true,
            "sort": "title_asc",
            "limit": 100
        }"#;
    let rules: CustomRules = serde_json::from_str(v1).unwrap();
    let RuleNode::All { children } = &rules.tree else {
        panic!("expected All root, got {:?}", rules.tree);
    };
    // 4 children: title, year_min, genre Any-group, liked.
    assert_eq!(children.len(), 4);
    // genre_ids must be wrapped in `Any` of three leaves.
    let genre_group = children.iter().find_map(|c| match c {
        RuleNode::Any { children } if children.len() == 3 => Some(children),
        _ => None,
    });
    assert!(
        genre_group.is_some(),
        "genre_ids should migrate to Any group of 3 leaves"
    );
    assert!(matches!(rules.sort, Some(CustomSort::TitleAsc)));
    assert_eq!(rules.limit, Some(100));
}

#[test]
fn v2_tree_round_trips() {
    let v2 = r#"{
            "tree": {
                "type": "all",
                "children": [
                    {"type": "leaf", "predicate": {"kind": "liked"}},
                    {"type": "not", "child": {"type": "leaf",
                        "predicate": {"kind": "hi_res"}}}
                ]
            },
            "sort": "random",
            "limit": 50
        }"#;
    let rules: CustomRules = serde_json::from_str(v2).unwrap();
    let RuleNode::All { children } = &rules.tree else {
        panic!("expected All root");
    };
    assert_eq!(children.len(), 2);
    assert!(matches!(rules.sort, Some(CustomSort::Random)));
}

use super::*;

/// The ordering guarantee, without a Tauri app: `order` is what the
/// status bar renders, and a `HashMap`'s iteration order would make
/// rows swap places between two ticks.
#[test]
fn rows_keep_their_insertion_order() {
    let mut inner = Inner::default();
    for (id, kind) in [
        (1u64, TaskKind::LibraryScan),
        (2, TaskKind::Analysis),
        (3, TaskKind::Backup),
    ] {
        inner.entries.insert(
            id,
            TaskEntry {
                kind,
                detail: None,
                current: 0,
                total: 0,
                cancel: Cancellation::Flag,
                cancelling: false,
                live: Arc::new(Mutex::new(true)),
            },
        );
        inner.order.push(id);
    }
    inner.entries.remove(&2);
    inner.order.retain(|id| *id != 2);

    let slugs: Vec<&str> = inner
        .order
        .iter()
        .filter_map(|id| inner.entries.get(id).map(|e| e.kind.slug()))
        .collect();
    assert_eq!(slugs, vec!["library_scan", "backup"]);
}

/// A kind's slug is a wire contract with the frontend's i18n keys.
/// Pinned here so renaming one is a deliberate act with a failing
/// test, not a refactor that silently unlabels a row.
#[test]
fn slugs_are_stable_and_distinct() {
    let all = [
        TaskKind::LibraryScan,
        TaskKind::Analysis,
        TaskKind::LyricsPrefetch,
        TaskKind::CatalogueMirror,
        TaskKind::Reconcile,
        TaskKind::Upload,
        TaskKind::Backup,
        TaskKind::Thumbnails,
        TaskKind::DropImport,
        TaskKind::Spectrogram,
        TaskKind::CacheMove,
    ];
    let mut seen = std::collections::HashSet::new();
    for kind in all {
        assert!(seen.insert(kind.slug()), "duplicate slug {}", kind.slug());
    }
    assert_eq!(seen.len(), 11);
    assert_eq!(TaskKind::LibraryScan.slug(), "library_scan");
    assert_eq!(TaskKind::CatalogueMirror.slug(), "catalogue_mirror");
}

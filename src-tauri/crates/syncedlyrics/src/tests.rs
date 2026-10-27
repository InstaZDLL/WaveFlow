use super::*;

#[test]
fn only_an_unreachable_provider_counts_as_transport() {
    assert!(Error::Transport {
        message: "connect timeout".into(),
        source: None,
    }
    .is_transport());
    assert!(!Error::Provider("stub page".into()).is_transport());
    let json = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
    assert!(!Error::Json(json).is_transport());
}

#[test]
fn a_miss_is_a_verdict_only_when_someone_answered() {
    let nobody = SearchReport {
        failures: vec![ProviderFailure {
            provider: Provider::Megalobiz,
            error: Error::Transport {
                message: "connect timeout".into(),
                source: None,
            },
        }],
        ..SearchReport::default()
    };
    assert!(!nobody.has_verdict());

    let partial = SearchReport {
        answered: vec![Provider::Lrclib],
        ..nobody
    };
    assert!(partial.has_verdict());
    assert!(!partial.failures.is_empty());
}

#[test]
fn detects_enhanced_lrc() {
    let content = "[00:01.00]<00:01.00>Hello <00:01.50>world";
    assert_eq!(detect_format(content), LyricsFormat::EnhancedLrc);
}

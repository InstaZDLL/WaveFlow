use super::*;

#[test]
fn interrupted_non_studio_launch_recovers() {
    let state = SkinState {
        skin: "lounge".into(),
        armed_by: Some(10),
        recovery_pending: false,
    };
    let (next, reason) = startup_state(state, false, true, 20, |_| false);
    assert_eq!(reason, RecoveryReason::PreviousExit);
    assert!(next.recovery_pending);
    assert_eq!(next.armed_by, None);
}

#[test]
fn second_launch_does_not_steal_live_marker() {
    let state = SkinState {
        skin: "liquid".into(),
        armed_by: Some(10),
        recovery_pending: false,
    };
    let (next, reason) = startup_state(state, false, true, 20, |_| true);
    assert_eq!(reason, RecoveryReason::None);
    assert_eq!(next.armed_by, Some(10));
}

#[test]
fn normal_launch_arms_non_studio_skin() {
    let state = SkinState {
        skin: "editorial".into(),
        armed_by: None,
        recovery_pending: false,
    };
    let (next, reason) = startup_state(state, false, true, 20, |_| false);
    assert_eq!(reason, RecoveryReason::None);
    assert_eq!(next.armed_by, Some(20));
}

#[test]
fn manual_recovery_also_works_outside_linux() {
    let (next, reason) = startup_state(SkinState::default(), true, false, 20, |_| false);
    assert_eq!(reason, RecoveryReason::Manual);
    assert!(next.recovery_pending);
}

#[test]
fn duplicate_manual_launch_does_not_reset_active_skin() {
    let overwritten = SkinState {
        skin: "lounge".into(),
        armed_by: None,
        recovery_pending: true,
    };
    let restored = restored_after_duplicate(overwritten, "lounge", false, 42);
    assert_eq!(restored.skin, "lounge");
    assert_eq!(restored.armed_by, Some(42));
    assert!(!restored.recovery_pending);
}

#[cfg(target_os = "linux")]
#[test]
fn updated_binary_still_counts_as_running() {
    assert!(same_executable_name(
        Path::new("/tmp/app/waveflow (deleted)"),
        Path::new("/usr/bin/waveflow")
    ));
}

use super::*;

fn state(remembered: Option<RenderMode>, armed: Option<RenderMode>) -> RenderState {
    RenderState {
        remembered,
        armed,
        armed_by: None,
    }
}

#[test]
fn a_first_launch_uses_the_gpu() {
    let decision = decide_from(&state(None, None), None, true);
    assert_eq!(decision.mode, RenderMode::Gpu);
    assert_eq!(decision.reason, RenderReason::Default);
    assert!(
        !decision.can_retry_gpu,
        "nothing to undo, so nothing is offered"
    );
}

/// The defect this exists for: the previous launch armed the GPU
/// path and never reported a paint.
#[test]
fn a_launch_that_never_painted_sends_the_next_one_to_software() {
    let decision = decide_from(&state(None, Some(RenderMode::Gpu)), None, true);
    assert_eq!(decision.mode, RenderMode::Software);
    assert_eq!(decision.reason, RenderReason::PreviousLaunchNeverPainted);
    assert!(decision.can_retry_gpu);
}

/// And it sticks. A one-shot fallback would paint once and then be
/// blank again on the launch after it — every other start.
#[test]
fn a_software_launch_that_painted_is_remembered() {
    let decision = decide_from(&state(Some(RenderMode::Software), None), None, true);
    assert_eq!(decision.mode, RenderMode::Software);
    assert_eq!(decision.reason, RenderReason::Remembered);
    assert!(decision.can_retry_gpu);
}

/// Software failing to paint says the GPU was not the problem.
/// Staying there would degrade rendering for a fault it does not
/// address.
#[test]
fn software_failing_too_goes_back_to_the_default() {
    let decision = decide_from(
        &state(Some(RenderMode::Software), Some(RenderMode::Software)),
        None,
        true,
    );
    assert_eq!(decision.mode, RenderMode::Gpu);
    assert_eq!(decision.reason, RenderReason::SoftwareDidNotHelp);
}

/// The file is the whole memory of this mechanism, so what it does
/// and does not carry between two launches is the contract.
#[test]
fn the_state_file_carries_exactly_what_the_next_launch_needs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("renderer.json");

    // Nothing written yet: the default, and no complaint about it.
    let (state, note) = read_state(&path);
    assert!(state.remembered.is_none() && state.armed.is_none());
    assert!(note.is_none(), "an absent file is not a problem");

    write_state(
        &path,
        &RenderState {
            remembered: Some(RenderMode::Software),
            armed: Some(RenderMode::Gpu),
            armed_by: None,
        },
    )
    .expect("write");
    let (state, note) = read_state(&path);
    assert_eq!(state.remembered, Some(RenderMode::Software));
    assert_eq!(state.armed, Some(RenderMode::Gpu));
    assert!(note.is_none());

    // Nothing left to say is the absence of the file, not an empty
    // one: the default has to survive the state being cleared.
    write_state(
        &path,
        &RenderState {
            remembered: None,
            armed: None,
            armed_by: None,
        },
    )
    .expect("clear");
    assert!(!path.exists());
    // And clearing what is already clear is not an error.
    write_state(
        &path,
        &RenderState {
            remembered: None,
            armed: None,
            armed_by: None,
        },
    )
    .expect("clear again");
}

/// The write replaces the file rather than truncating it, and
/// leaves nothing beside it — the temporary is the kind of litter
/// that ends up in someone's app-data directory forever.
#[test]
fn writing_the_state_leaves_one_file_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("renderer.json");

    for armed in [Some(RenderMode::Gpu), Some(RenderMode::Software), None] {
        write_state(
            &path,
            &RenderState {
                remembered: Some(RenderMode::Software),
                armed,
                armed_by: None,
            },
        )
        .expect("write");
    }

    let left: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read_dir")
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
        .collect();
    assert_eq!(left, vec!["renderer.json".to_string()]);
}

/// A file truncated by a crash mid-write, or edited by hand, must
/// read as "nothing known" and say so — never refuse the launch.
/// This runs before a window exists, so an error here is a startup
/// that does not happen.
#[test]
fn an_unreadable_state_file_is_a_default_and_a_complaint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("renderer.json");
    std::fs::write(&path, "{\"remembered\": \"softw").expect("write");

    let (state, note) = read_state(&path);
    assert!(state.remembered.is_none() && state.armed.is_none());
    assert!(note.is_some(), "the one line that explains the default");
}

/// The escape hatch wins over everything the file says, in both
/// directions.
#[test]
fn the_environment_decides_when_it_is_set() {
    let armed = state(Some(RenderMode::Software), Some(RenderMode::Gpu));
    for forced in [RenderMode::Gpu, RenderMode::Software] {
        let decision = decide_from(&armed, Some(forced), true);
        assert_eq!(decision.mode, forced);
        assert_eq!(decision.reason, RenderReason::Forced);
        assert!(
            !decision.can_retry_gpu,
            "the file is not what is deciding, so the button would change nothing"
        );
    }
}

/// macOS has no environment switch for WKWebView compositing, so
/// the fallback there would be a mode nothing implements: a banner
/// announcing a downgrade that never happened, and a stored state
/// that never lets the GPU be tried again. Every route into
/// software has to answer the same way.
#[test]
fn a_platform_without_a_software_path_never_claims_to_use_one() {
    let cases = [
        ("the escalation", state(None, Some(RenderMode::Gpu)), None),
        (
            "a remembered fallback",
            state(Some(RenderMode::Software), None),
            None,
        ),
        (
            "an explicit request",
            state(None, None),
            Some(RenderMode::Software),
        ),
    ];
    for (label, state, forced) in cases {
        let decision = decide_from(&state, forced, false);
        assert_eq!(decision.mode, RenderMode::Gpu, "{label}");
        assert_eq!(
            decision.reason,
            RenderReason::SoftwareUnavailable,
            "{label}"
        );
        assert!(!decision.can_retry_gpu, "{label}");
    }
}

/// And asking for the GPU there is still an ordinary answer — only
/// the software half is missing.
#[test]
fn the_gpu_can_still_be_forced_without_a_software_path() {
    let decision = decide_from(&state(None, None), Some(RenderMode::Gpu), false);
    assert_eq!(decision.mode, RenderMode::Gpu);
    assert_eq!(decision.reason, RenderReason::Forced);
}

fn armed_by(pid: u32) -> RenderState {
    RenderState {
        remembered: None,
        armed: Some(RenderMode::Gpu),
        armed_by: Some(pid),
    }
}

/// #755: two launches a few milliseconds apart. The second finds the
/// first's marker armed because the first has not painted *yet* —
/// and must not take that for a launch that failed.
#[test]
fn a_marker_armed_by_a_launch_still_running_is_not_a_failure() {
    let other = std::process::id().wrapping_add(1);
    let (state, note) = without_live_marker(armed_by(other), |pid| pid == other);
    assert!(state.armed.is_none() && state.armed_by.is_none());
    assert!(note.is_some(), "the log says why the marker was set aside");
    let decision = decide_from(&state, None, true);
    assert_eq!(decision.mode, RenderMode::Gpu);
    assert_eq!(decision.reason, RenderReason::Default);
}

/// The case the fallback exists for is untouched: the launch that
/// armed the marker is gone.
#[test]
fn a_marker_armed_by_a_launch_that_is_gone_still_falls_back() {
    let other = std::process::id().wrapping_add(1);
    let (state, note) = without_live_marker(armed_by(other), |_| false);
    assert!(note.is_none());
    let decision = decide_from(&state, None, true);
    assert_eq!(decision.reason, RenderReason::PreviousLaunchNeverPainted);
}

/// A file written before the process was recorded keeps the meaning
/// every marker had then, whatever the probe would say.
#[test]
fn a_marker_with_no_process_keeps_its_old_meaning() {
    let (state, note) = without_live_marker(state(None, Some(RenderMode::Gpu)), |_| true);
    assert_eq!(state.armed, Some(RenderMode::Gpu));
    assert!(note.is_none());
}

/// What a marker records about its process has to survive the file.
#[test]
fn the_arming_process_is_written_and_read_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("renderer.json");
    write_state(
        &path,
        &RenderState::written_here(None, Some(RenderMode::Gpu)),
    )
    .expect("write");
    let (state, _) = read_state(&path);
    assert_eq!(state.armed_by, Some(std::process::id()));

    // Disarmed, it names nobody.
    let cleared = RenderState::written_here(Some(RenderMode::Software), None);
    assert!(cleared.armed_by.is_none());
}

/// The probe itself, on the two platforms that have one: this
/// process is a running launch of this executable, and a pid no
/// system hands out is not.
#[cfg(any(target_os = "linux", target_os = "windows"))]
#[test]
fn the_probe_recognises_a_running_launch() {
    assert!(launch_in_progress(std::process::id()));
    assert!(!launch_in_progress(u32::MAX - 1));
}

#[test]
fn executables_are_compared_by_name() {
    assert!(same_executable(
        Path::new("/tmp/.mount_a/usr/bin/waveflow"),
        Path::new("/tmp/.mount_b/usr/bin/waveflow"),
    ));
    assert!(same_executable(
        Path::new("/usr/bin/waveflow (deleted)"),
        Path::new("/usr/bin/waveflow"),
    ));
    assert!(same_executable(
        Path::new("C:/Program Files/WaveFlow/WaveFlow.exe"),
        Path::new("C:/Program Files/WaveFlow/waveflow.exe"),
    ));
    assert!(!same_executable(
        Path::new("/usr/bin/firefox"),
        Path::new("/usr/bin/waveflow"),
    ));
}

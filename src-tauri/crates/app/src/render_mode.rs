//! Choosing a renderer, and surviving one that cannot paint (#595).
//!
//! When GPU-accelerated rendering fails, the app starts and shows
//! nothing: the window exists, the process is alive, and the user has a
//! blank rectangle with no message and no way back. The AppImage
//! `libwayland-client` fix (#525) removed one cause of that; it did not
//! make the class of failure survivable.
//!
//! ## The sentinel
//!
//! A launch **arms** a marker before the window is created and disarms
//! it once the interface has actually painted. A launch that finds the
//! marker still armed knows the previous one never got that far, and
//! starts in software rendering instead.
//!
//! The signal to disarm on already exists: the frontend reports its
//! first committed render through [`crate::commands::ready`], which the
//! splash handoff depends on. Nothing new has to be instrumented.
//!
//! **Paint is the only thing that disarms it.** Not window close, not
//! process exit. Clearing the marker when the user closes the blank
//! window would let them erase the evidence themselves, and the next
//! launch would try the GPU path again — the loop repeating forever
//! while the mechanism appears not to work.
//!
//! ## Why the fallback sticks
//!
//! A one-shot fallback would be worse than none: the software launch
//! paints, the marker clears, and the launch after it is blank again —
//! every other start of the app. So a software launch that painted
//! records that it was software, and later launches go straight there.
//! [`retry_gpu`] is the way back, and the interface offers it.
//!
//! The one case that does not stick is software *also* failing to
//! paint. The GPU was then not the problem, and staying in software
//! would degrade rendering for a fault it does not address — so that
//! goes back to the default and says so in the log.
//!
//! ## What "software" actually sets
//!
//! Environment variables, read by the web engine when its process
//! starts, which is why this has to run before any window exists.
//! Measured against the engines in play rather than copied from advice:
//!
//! - **Linux / WebKitGTK** — `WEBKIT_DISABLE_COMPOSITING_MODE` and
//!   `WEBKIT_DISABLE_DMABUF_RENDERER` are both read by the installed
//!   library (2.52, checked in its symbol table), and
//!   `LIBGL_ALWAYS_SOFTWARE` is Mesa's own switch to llvmpipe.
//! - **Windows / WebView2** — `--disable-gpu`, appended through
//!   `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, which the runtime reads
//!   when it creates its environment. Microsoft documents one
//!   limitation worth knowing: **an elevated app ignores flags set this
//!   way**, so a WaveFlow started as administrator cannot be rescued by
//!   this path.
//! - **macOS / WKWebView** — nothing equivalent exists, and inventing a
//!   half-answer would be worse than the honest none.
//!
//! A variable the user already set is never overwritten: someone who
//! exported one of these did so deliberately, and this is not the place
//! to argue with them.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The environment variable that overrides everything below.
///
/// The escape hatch: a user who has been told what to try can try it
/// without editing a file or finding a setting.
pub const OVERRIDE_VAR: &str = "WAVEFLOW_RENDERER";

/// How the interface is being drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderMode {
    /// The default: whatever the platform's web engine does on its own.
    Gpu,
    /// Compositing and hardware GL turned off.
    Software,
}

impl RenderMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Software => "software",
        }
    }
}

/// Why the mode above was chosen — the part the user is owed when the
/// answer is not the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RenderReason {
    /// Nothing asked for anything else.
    Default,
    /// `WAVEFLOW_RENDERER` named it.
    Forced,
    /// The previous launch armed the marker on the GPU path and never
    /// painted. This is the fallback doing its job, and the one case
    /// the interface explains unprompted.
    PreviousLaunchNeverPainted,
    /// A software launch painted, so this one did not even try the GPU.
    Remembered,
    /// Software did not paint either, so the GPU was not the problem
    /// and this is back on the default.
    SoftwareDidNotHelp,
    /// The fallback was called for and this platform has none, so
    /// nothing changed. Said rather than hidden: a launch that keeps
    /// coming up blank is worth a reason, even when the reason is that
    /// there is nothing here to try.
    SoftwareUnavailable,
}

/// Whether [`apply`] has anything to set on this platform.
///
/// macOS is the `false`: WKWebView has no environment switch for
/// compositing, so answering `Software` there would be a mode nothing
/// implements — a banner announcing a downgrade that never happened,
/// and a stored state that never lets the GPU be tried again.
const SOFTWARE_AVAILABLE: bool = !cfg!(target_os = "macos");

/// What was decided, and what the interface is told about it.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderDecision {
    pub mode: RenderMode,
    pub reason: RenderReason,
    /// Whether [`retry_gpu`] has anything to undo. False when the mode
    /// was forced by the environment — the file is not what is deciding
    /// then, and offering a button that changes nothing would be a lie.
    pub can_retry_gpu: bool,
}

/// What the previous launch left behind.
#[derive(Debug, Default, Serialize, Deserialize)]
struct RenderState {
    /// Set once a **software** launch painted, and read by every launch
    /// after it. Absent means the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    remembered: Option<RenderMode>,
    /// The mode this launch is attempting, removed the moment it
    /// paints. Still here on the next launch means it never did.
    #[serde(skip_serializing_if = "Option::is_none")]
    armed: Option<RenderMode>,
}

/// Where the state lives and what was decided, for the two callers that
/// come later: the paint signal, and the commands the interface uses.
struct Active {
    path: PathBuf,
    decision: RenderDecision,
}

static ACTIVE: std::sync::OnceLock<Active> = std::sync::OnceLock::new();

/// Whether this instance has reported a paint, for the one caller that
/// has to know without writing anything itself.
static PAINTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn state_path(root: &Path) -> PathBuf {
    root.join("renderer.json")
}

fn read_state(path: &Path) -> RenderState {
    // Anything unreadable — absent, truncated by a crash mid-write,
    // hand-edited into nonsense — reads as "nothing known". This runs
    // before the window exists, so a parse error here is a startup that
    // never happens; the default is the safe answer and the log says it
    // was taken.
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|err| {
            tracing::warn!(%err, path = %path.display(), "renderer state unreadable; starting from the default");
            RenderState::default()
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => RenderState::default(),
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "renderer state could not be read");
            RenderState::default()
        }
    }
}

/// Write the state, or say why it could not be written.
///
/// Not fsync'd, and that is deliberate. What has to survive is a *web
/// engine process* dying, not the machine losing power — and the bytes
/// are in the page cache the moment the write returns, which the next
/// process reads. Paying for a flush on every launch would buy nothing
/// this mechanism needs.
fn write_state(path: &Path, state: &RenderState) {
    if state.remembered.is_none() && state.armed.is_none() {
        // Nothing left to say: the default is the absence of the file.
        if let Err(err) = std::fs::remove_file(path) {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%err, "could not clear the renderer state");
            }
        }
        return;
    }
    let Ok(raw) = serde_json::to_string(state) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(err) = std::fs::write(path, raw) {
        // Non-fatal on purpose. A read-only app-data directory is a
        // problem the user has already, and refusing to start over it
        // would turn a degraded launch into no launch at all. The cost
        // is that the fallback cannot arm — which is where we were
        // before this existed.
        tracing::warn!(%err, path = %path.display(), "could not write the renderer state");
    }
}

/// Read `WAVEFLOW_RENDERER`. Anything but the three words it accepts is
/// reported and ignored — a typo should not silently mean something.
fn override_from_env() -> Option<RenderMode> {
    let raw = std::env::var(OVERRIDE_VAR).ok()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "gpu" => Some(RenderMode::Gpu),
        "software" => Some(RenderMode::Software),
        "auto" | "" => None,
        other => {
            tracing::warn!(
                value = other,
                "{OVERRIDE_VAR} is not one of gpu / software / auto; ignoring it"
            );
            None
        }
    }
}

/// Work out what the previous launch implies, with no side effects, so
/// the table above is testable without a filesystem or a process
/// environment.
///
/// `software_available` is a parameter rather than a `cfg!` read inside
/// the body for the same reason: the platform that has no software path
/// is the one with no CI job, and a branch only reachable there is a
/// branch nobody runs.
fn decide_from(
    state: &RenderState,
    forced: Option<RenderMode>,
    software_available: bool,
) -> RenderDecision {
    // Asking for something this build cannot do is answered, not
    // silently granted: the caller would otherwise be told it is in
    // software rendering while nothing had been turned off.
    let unavailable = RenderDecision {
        mode: RenderMode::Gpu,
        reason: RenderReason::SoftwareUnavailable,
        can_retry_gpu: false,
    };

    if let Some(mode) = forced {
        if mode == RenderMode::Software && !software_available {
            return unavailable;
        }
        return RenderDecision {
            mode,
            reason: RenderReason::Forced,
            can_retry_gpu: false,
        };
    }
    match state.armed {
        // The previous launch never painted. What it was attempting
        // decides what this one does.
        Some(RenderMode::Gpu) if !software_available => unavailable,
        Some(RenderMode::Gpu) => RenderDecision {
            mode: RenderMode::Software,
            reason: RenderReason::PreviousLaunchNeverPainted,
            can_retry_gpu: true,
        },
        Some(RenderMode::Software) => RenderDecision {
            mode: RenderMode::Gpu,
            reason: RenderReason::SoftwareDidNotHelp,
            can_retry_gpu: false,
        },
        None => match state.remembered {
            // A state file can outlive the build that wrote it — copied
            // between machines, or carried across an update that
            // dropped the platform's software path.
            Some(RenderMode::Software) if !software_available => unavailable,
            Some(RenderMode::Software) => RenderDecision {
                mode: RenderMode::Software,
                reason: RenderReason::Remembered,
                can_retry_gpu: true,
            },
            // `remembered: gpu` is never written — the default already
            // means that — but a hand-edited file could say it, and the
            // answer is the same either way.
            _ => RenderDecision {
                mode: RenderMode::Gpu,
                reason: RenderReason::Default,
                can_retry_gpu: false,
            },
        },
    }
}

/// Set a variable the web engine reads, unless the user already set it.
///
/// Linux only, because it is where the several-variables shape applies;
/// Windows appends to one variable instead, and macOS has none. Gated
/// rather than left general: an unused helper is dead code on the two
/// platforms that do not call it, and CI only lints one of them.
#[cfg(target_os = "linux")]
fn set_unless_present(key: &str, value: &str) {
    if std::env::var_os(key).is_some() {
        tracing::debug!(key, "already set in the environment; leaving it alone");
        return;
    }
    std::env::set_var(key, value);
}

/// Turn the decision into the environment the web engine will read.
fn apply(mode: RenderMode) {
    if mode == RenderMode::Gpu {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        set_unless_present("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        set_unless_present("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        set_unless_present("LIBGL_ALWAYS_SOFTWARE", "1");
    }
    #[cfg(target_os = "windows")]
    {
        const KEY: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";
        const FLAG: &str = "--disable-gpu";
        // Appended rather than assigned: the variable may already carry
        // arguments — the runtime reads whatever is there — and
        // replacing it would drop them.
        match std::env::var(KEY) {
            Ok(existing) if existing.split_whitespace().any(|arg| arg == FLAG) => {}
            Ok(existing) if !existing.trim().is_empty() => {
                std::env::set_var(KEY, format!("{} {FLAG}", existing.trim()));
            }
            _ => std::env::set_var(KEY, FLAG),
        }
    }
}

/// Decide, arm the marker, and set the environment — in that order,
/// before any window exists.
///
/// Returns what was decided. The caller holds it only for logging; the
/// commands read it back through [`current`].
pub fn decide(root: PathBuf) -> RenderDecision {
    let path = state_path(&root);
    let previous = read_state(&path);
    let decision = decide_from(&previous, override_from_env(), SOFTWARE_AVAILABLE);

    // What this launch is attempting, written before the window can
    // fail to paint. The remembered mode is carried through: a launch
    // that was told to use the GPU by the environment must not erase
    // what the file knows, or the override would quietly become
    // permanent the moment it was removed.
    let remembered = match decision.reason {
        // Software did not help either, so what was remembered is
        // wrong and goes.
        RenderReason::SoftwareDidNotHelp => None,
        _ => previous.remembered,
    };
    write_state(
        &path,
        &RenderState {
            remembered,
            armed: Some(decision.mode),
        },
    );

    apply(decision.mode);

    tracing::info!(
        mode = decision.mode.as_str(),
        reason = ?decision.reason,
        "renderer selected"
    );
    let _ = ACTIVE.set(Active { path, decision });
    decision
}

/// What was decided for this launch, once [`decide`] has run.
pub fn current() -> Option<RenderDecision> {
    ACTIVE.get().map(|active| active.decision)
}

/// The interface painted: disarm the marker, and remember the mode if
/// it was the fallback.
///
/// Called from the paint signal and from nowhere else — see the note at
/// the top of this module about why closing the window must not reach
/// here. Idempotent: the signal has two transports and either may
/// arrive first, or both.
pub fn mark_painted() {
    PAINTED.store(true, std::sync::atomic::Ordering::Release);
    let Some(active) = ACTIVE.get() else {
        return;
    };
    write_state(
        &active.path,
        &RenderState {
            remembered: remembered_after_paint(active),
            armed: None,
        },
    );
}

/// What the file should remember, once this launch has painted.
///
/// A mode the **environment** forced says nothing about what the app
/// would have chosen, so it leaves what the file already knew alone.
/// Deriving from the mode instead made `WAVEFLOW_RENDERER=software`
/// permanent: one launch under it wrote the fallback into the state,
/// and removing the variable changed nothing afterwards. The arming
/// side of `decide` already refused to do that; this side did not.
fn remembered_after_paint(active: &Active) -> Option<RenderMode> {
    if active.decision.reason == RenderReason::Forced {
        return read_state(&active.path).remembered;
    }
    match active.decision.mode {
        // Worth remembering: the next launch should not have to fail
        // once more to learn what this one just proved.
        RenderMode::Software => Some(RenderMode::Software),
        // The GPU path works, so the file has nothing left to say.
        RenderMode::Gpu => None,
    }
}

/// Put back what a **duplicate launch** overwrote (#595).
///
/// The single-instance plugin turns a second launch away from inside
/// `Builder`, which is after this module has already armed the marker
/// in that process. So opening the app twice left a marker armed by a
/// process that was never going to paint, and the launch after it fell
/// back to software for no reason at all.
///
/// Called from the plugin's callback, which runs **in the instance that
/// is staying**, and writes what that instance knows to be true: it has
/// painted, or it is still the one attempting its own mode.
pub fn restore_after_duplicate_launch() {
    let Some(active) = ACTIVE.get() else {
        return;
    };
    let painted = PAINTED.load(std::sync::atomic::Ordering::Acquire);
    write_state(
        &active.path,
        &RenderState {
            remembered: if painted {
                remembered_after_paint(active)
            } else {
                read_state(&active.path).remembered
            },
            armed: (!painted).then_some(active.decision.mode),
        },
    );
    tracing::debug!(painted, "renderer state restored after a duplicate launch");
}

/// Forget that software rendering was ever needed, so the next launch
/// tries the GPU again.
///
/// The way back for a user whose hardware was fixed, whose driver was
/// updated, or who hit the fallback once for an unrelated reason —
/// a force-quit during startup looks exactly like a GPU that cannot
/// paint, and nothing can tell them apart from here.
pub fn retry_gpu() {
    let Some(active) = ACTIVE.get() else {
        return;
    };
    // The armed marker for the *current* launch stays: this launch has
    // painted or it has not, and that question is not what is being
    // answered here.
    let armed = read_state(&active.path).armed;
    write_state(
        &active.path,
        &RenderState {
            remembered: None,
            armed,
        },
    );
    tracing::info!("renderer: forgot the software fallback; the next launch will try the GPU");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(remembered: Option<RenderMode>, armed: Option<RenderMode>) -> RenderState {
        RenderState { remembered, armed }
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
}

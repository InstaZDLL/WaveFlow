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
//! ## Two launches at once
//!
//! A marker still armed does not always mean a launch died: it can
//! belong to one that is **still starting**, beside this one. Opening the
//! app twice within a few milliseconds — a double click is enough —
//! runs two processes through this module before the single-instance
//! plugin sorts them out, and the second read the first's marker as a
//! launch that never painted (#755). When the second is the one that
//! stays, it started in software for nothing and, having painted there,
//! remembered it. So the marker carries the process that armed it, and
//! one armed by a WaveFlow launch that is still running is not evidence
//! of anything.
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
//! ## What this catches, and what it does not
//!
//! The marker is disarmed by the frontend reporting its first committed
//! render, which is the strongest signal this side has and is not the
//! same thing as pixels reaching the screen. A failure that leaves
//! JavaScript running while nothing composites would still report a
//! paint, and the fallback would not engage — what it catches is the
//! shape actually reported: a launch that gets far enough to open a
//! window and never gets far enough to render into it.
//!
//! Waiting for the native reveal instead would not fix that: `show()`
//! returning `Ok` proves no more about pixels than a React commit does.
//! Nothing available here can prove it, so the honest answer is to say
//! which signal is used and what it means.
//!
//! It cuts the other way too. The splash's 15-second safety net reveals
//! the window when no signal arrives, and it deliberately does **not**
//! disarm the marker — a renderer that cannot paint produces exactly
//! that timeout, so disarming there would make this inert. The cost is
//! that a signal lost for some other reason (#626's original defect,
//! now carried by two transports) reads as a launch that never
//! painted, and the launch after it falls back. That is not silent: it
//! is the case the banner explains and the retry button undoes.
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
//! A variable the user already set is left as it is — someone who
//! exported one of these did so deliberately, and this is not the place
//! to argue with them. The Windows one is the exception, and has to be:
//! `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` is a list of arguments, so
//! the flag is **appended** to whatever is already there rather than
//! replacing it. Nothing the user put in it is lost; the variable
//! itself does change.

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
    /// The process that armed it, so a launch can tell a marker left by
    /// one that died without painting from one that is still starting
    /// up beside it (#755). Absent from a file written before this
    /// existed, which reads as the former — what every marker meant then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    armed_by: Option<u32>,
}

impl RenderState {
    /// A state this process writes: any marker in it is this process's.
    fn written_here(remembered: Option<RenderMode>, armed: Option<RenderMode>) -> Self {
        Self {
            remembered,
            armed,
            armed_by: armed.map(|_| std::process::id()),
        }
    }
}

/// Where the state lives and what was decided, for the two callers that
/// come later: the paint signal, and the commands the interface uses.
struct Active {
    path: PathBuf,
    decision: RenderDecision,
    /// What [`decide`] would have logged had there been anywhere to log
    /// it. Read by [`log_decision`].
    notes: Vec<String>,
    /// What the file remembered when this launch read it, carried so
    /// nothing later has to read it again.
    ///
    /// A **second launch also runs `decide`** — the single-instance
    /// plugin turns it away from inside `Builder`, long after this — and
    /// it reads the marker this instance armed. Seeing `armed: software`
    /// it concludes software did not help and writes `remembered: null`,
    /// erasing a working fallback that belonged to the instance still
    /// starting up. Every write after the decision uses this value
    /// instead of whatever the file says by then, so the duplicate's
    /// guess cannot outlive it.
    remembered: Option<RenderMode>,
}

static ACTIVE: std::sync::OnceLock<Active> = std::sync::OnceLock::new();

/// Whether this instance has reported a paint, for the one caller that
/// has to know without writing anything itself.
static PAINTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the user asked for the GPU back during this session.
static RETRY_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Serialises every write that happens **after** the decision.
///
/// Each of them is a read-modify-write of one small file, and they run
/// on different threads: the paint signal arrives on a command handler,
/// the duplicate-launch restore on the event loop, the retry on another
/// command. Without this, a second launch landing in the instant the
/// first one paints could read "not painted yet", then write its marker
/// back over the disarm that had just happened — arming a launch that
/// had, in fact, painted.
///
/// [`decide`] does not take it: nothing else exists yet when it runs.
static STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Hold [`STATE_LOCK`] across a read-modify-write, surviving a poisoned
/// lock: what it guards is a file, not an invariant a panic could have
/// left half-built.
fn locked<T>(body: impl FnOnce() -> T) -> T {
    let _guard = STATE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    body()
}

fn state_path(root: &Path) -> PathBuf {
    root.join("renderer.json")
}

/// Read the file, and say what went wrong rather than logging it.
///
/// Anything unreadable — absent, truncated by a crash mid-write,
/// hand-edited into nonsense — reads as "nothing known": the default is
/// the safe answer, and refusing to start over a state file would be
/// worse than the blank window this exists to survive.
///
/// The complaint is **returned** because the first call happens before
/// the logging subscriber exists — see [`decide`] — and a `warn!` there
/// would go nowhere at all. The caller that runs later logs it as it
/// arrives.
fn read_state(path: &Path) -> (RenderState, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str(&raw) {
            Ok(state) => (state, None),
            Err(err) => (
                RenderState::default(),
                Some(format!(
                    "renderer state unreadable, using the default: {err}"
                )),
            ),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (RenderState::default(), None),
        Err(err) => (
            RenderState::default(),
            Some(format!("renderer state could not be read: {err}")),
        ),
    }
}

/// Write the state, or say why it could not be written.
///
/// Not fsync'd, and that is deliberate. What has to survive is a *web
/// engine process* dying, not the machine losing power — and the bytes
/// are in the page cache the moment the write returns, which the next
/// process reads. Paying for a flush on every launch would buy nothing
/// this mechanism needs.
fn write_state(path: &Path, state: &RenderState) -> std::io::Result<()> {
    if state.remembered.is_none() && state.armed.is_none() {
        // Nothing left to say: the default is the absence of the file.
        return match std::fs::remove_file(path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        };
    }
    let raw = serde_json::to_string(state)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    let Some(parent) = path.parent() else {
        return std::fs::write(path, raw);
    };
    let _ = std::fs::create_dir_all(parent);

    // Written beside the target and renamed over it, rather than
    // truncated and refilled. `fs::write` opens with `truncate`, so a
    // process dying between the two leaves a file that parses as
    // nothing — and the process dying mid-startup is the exact event
    // this whole mechanism exists to notice. The rename is the atomic
    // step on both platforms that have one (`MoveFileEx` with
    // `REPLACE_EXISTING` underneath on Windows), and the temporary sits
    // in the same directory because a rename is only atomic within a
    // filesystem.
    //
    // No `fsync`, and no claim to survive a power cut: what has to
    // survive is a process, and the bytes are in the page cache the
    // moment the write returns.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temp = parent.join(format!(
        ".renderer.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&temp, raw)?;
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            // Nothing left behind beside the user's own files.
            let _ = std::fs::remove_file(&temp);
            Err(err)
        }
    }
}

/// Write it, and carry on if it could not be written.
///
/// For the callers on the startup path, where the failure is real but
/// the answer to it is not to refuse to start: a read-only app-data
/// directory is a problem the user has already, and turning a degraded
/// launch into no launch would be worse. The cost is that the fallback
/// cannot arm — which is exactly where the app was before any of this
/// existed. The one caller that does **not** use this is the retry
/// button, because a button may not report a success it did not have.
fn write_state_best_effort(path: &Path, state: &RenderState) {
    if let Err(err) = write_state(path, state) {
        tracing::warn!(%err, path = %path.display(), "could not write the renderer state");
    }
}

/// Read `WAVEFLOW_RENDERER`. Anything but the three words it accepts is
/// reported and ignored — a typo should not silently mean something,
/// and the person who made it is the one person guaranteed to be
/// reading the log.
///
/// The complaint is returned for the same reason [`read_state`]'s is:
/// this runs before the logging subscriber exists.
fn override_from_env() -> (Option<RenderMode>, Option<String>) {
    let Ok(raw) = std::env::var(OVERRIDE_VAR) else {
        return (None, None);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "gpu" => (Some(RenderMode::Gpu), None),
        "software" => (Some(RenderMode::Software), None),
        "auto" | "" => (None, None),
        other => (
            None,
            Some(format!(
                "{OVERRIDE_VAR} is {other:?}, not one of gpu / software / auto; ignoring it"
            )),
        ),
    }
}

/// Set aside a marker armed by a launch that is **still running**.
///
/// Two launches a few milliseconds apart both run [`decide`] before the
/// single-instance plugin turns one of them away, and the second finds
/// the first's marker armed — not because that launch failed to paint,
/// but because it has not had the time to (#755). Read as a failure, it
/// sent the second into software rendering; and when the second was the
/// one that stayed, it painted there and remembered it, for every launch
/// after.
///
/// `is_running` is the probe, passed in so the rule is testable without
/// spawning a process. A marker with no process recorded — written
/// before this existed — keeps its old meaning. Returns the note the log
/// is owed, since this runs before the log exists.
fn without_live_marker(
    previous: RenderState,
    is_running: impl Fn(u32) -> bool,
) -> (RenderState, Option<String>) {
    match previous.armed_by {
        Some(pid) if previous.armed.is_some() && pid != std::process::id() && is_running(pid) => (
            RenderState {
                armed: None,
                armed_by: None,
                ..previous
            },
            Some(format!(
                "the renderer marker belongs to another launch that is still starting (pid {pid}), not to one that failed to paint"
            )),
        ),
        _ => (previous, None),
    }
}

/// Whether `pid` is a WaveFlow launch that is still running.
///
/// The executable is compared, not only the process's existence: a pid
/// is handed out again once its process is gone, and the marker of a
/// launch that really died would otherwise be excused by whatever
/// program inherited its number. By file name rather than full path,
/// because an AppImage mounts itself somewhere new on every launch.
///
/// macOS answers `false`, which is what every marker meant before this
/// existed: it has no software path, so the question never changes its
/// decision there.
fn launch_in_progress(pid: u32) -> bool {
    let (Some(theirs), Ok(ours)) = (executable_of(pid), std::env::current_exe()) else {
        return false;
    };
    same_executable(&theirs, &ours)
}

fn same_executable(a: &Path, b: &Path) -> bool {
    // A binary replaced by an update while it runs reads back from
    // `/proc` with this suffix.
    let name = |path: &Path| {
        path.file_name().map(|name| {
            name.to_string_lossy()
                .trim_end_matches(" (deleted)")
                .to_lowercase()
        })
    };
    matches!((name(a), name(b)), (Some(x), Some(y)) if x == y)
}

/// The executable of a running process, or `None` when it has exited or
/// cannot be inspected.
#[cfg(target_os = "linux")]
fn executable_of(pid: u32) -> Option<PathBuf> {
    // Unreadable for a zombie — exited, not yet reaped — which is the
    // answer wanted: it will never paint.
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "windows")]
fn executable_of(pid: u32) -> Option<PathBuf> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    // SAFETY: the handle is opened here, used only by the calls below
    // and closed before returning; the buffer outlives the call that
    // fills it, and `len` carries its capacity in and the written
    // length out, as the API documents.
    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
        .ok()?;
        let path = (|| {
            // A handle can still be opened on a process that has exited
            // while something holds a reference to it. A process object
            // is signalled once it exits, so a zero wait that times out
            // is "still running". Not `GetExitCodeProcess`: its
            // `STILL_ACTIVE` is 259, which is also an exit code a process
            // can return, and a launch that died with it would read as
            // running — excusing the very marker the fallback needs.
            if WaitForSingleObject(handle, 0) != WAIT_TIMEOUT {
                return None;
            }
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .ok()?;
            Some(PathBuf::from(String::from_utf16_lossy(
                &buf[..len as usize],
            )))
        })();
        let _ = CloseHandle(handle);
        path
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn executable_of(_pid: u32) -> Option<PathBuf> {
    None
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
    let (previous, state_note) = read_state(&path);
    let (previous, concurrent_note) = without_live_marker(previous, launch_in_progress);
    let (forced, env_note) = override_from_env();
    let decision = decide_from(&previous, forced, SOFTWARE_AVAILABLE);
    let mut notes: Vec<String> = [env_note, state_note, concurrent_note]
        .into_iter()
        .flatten()
        .collect();

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
    // Not the best-effort form: that one logs, and there is nowhere to
    // log yet. This is the message that matters most of the three
    // deferred here — a marker that could not be armed means the whole
    // mechanism is inert for this launch, silently, and the log is the
    // only place that could ever say so.
    if let Err(err) = write_state(
        &path,
        &RenderState::written_here(remembered, Some(decision.mode)),
    ) {
        notes.push(format!(
            "could not arm the renderer marker, so this launch is not covered by the fallback: {err}"
        ));
    }

    apply(decision.mode);

    let _ = ACTIVE.set(Active {
        path,
        decision,
        notes,
        remembered,
    });
    decision
}

/// Say what was decided, once there is somewhere to say it.
///
/// Split from [`decide`] because that has to run **before** logging is
/// initialised: `tracing_appender` starts a worker thread, and the
/// environment this mutates is process-wide — changing it while another
/// thread might read it is the kind of race that is fine until it is
/// not. Deciding first and reporting second costs one call and settles
/// the question.
pub fn log_decision() {
    let Some(active) = ACTIVE.get() else {
        return;
    };
    for note in &active.notes {
        tracing::warn!(note, "renderer");
    }
    tracing::info!(
        mode = active.decision.mode.as_str(),
        reason = ?active.decision.reason,
        "renderer selected"
    );
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
    locked(|| {
        write_state_best_effort(
            &active.path,
            &RenderState::written_here(remembered_after_paint(active), None),
        );
    });
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
    // The user asked for the GPU back during this session, and a paint
    // reported after that must not quietly put the fallback back. Only
    // the startup paint can reach here today — the mini-player renders
    // without `ReadySignal`, so it does not signal — but "how many
    // times can this fire" is a worse question to depend on than a flag
    // that answers it.
    if RETRY_REQUESTED.load(std::sync::atomic::Ordering::Acquire) {
        return None;
    }
    if active.decision.reason == RenderReason::Forced {
        return active.remembered;
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
///
/// The duplicate cannot paint — the plugin is registered first and
/// exits it before any window is shown — so it never reaches
/// [`mark_painted`], and its own write is always followed by this one:
/// its decision happens before it reaches `Builder`, and this runs
/// because it reached `Builder`. What it wrote in between is
/// overwritten by the instance that is still here. The process stamped
/// into the marker plays no part in that; it answers the other
/// direction — the second launch *deciding* from the first one's marker
/// (#755), see [`without_live_marker`].
pub fn restore_after_duplicate_launch() {
    let Some(active) = ACTIVE.get() else {
        return;
    };
    // Read inside the lock, so a paint landing in this instant cannot be
    // overwritten by the state it was true in a moment ago.
    let painted = locked(|| {
        let painted = PAINTED.load(std::sync::atomic::Ordering::Acquire);
        write_state_best_effort(
            &active.path,
            &RenderState::written_here(
                if painted {
                    remembered_after_paint(active)
                } else {
                    active.remembered
                },
                (!painted).then_some(active.decision.mode),
            ),
        );
        painted
    });
    tracing::debug!(painted, "renderer state restored after a duplicate launch");
}

/// Take the marker down because this launch is stopping on purpose.
///
/// The marker means "a launch opened a window and never rendered into
/// it". A startup that refuses to continue — a database written by a
/// newer build, say — never opened one, and says nothing whatsoever
/// about the renderer. Left armed, it would send the next launch into
/// software rendering because of a schema version.
///
/// Reachable because the decision now runs *first*: it has to precede
/// the logger, which the fatal paths need in order to explain
/// themselves.
pub fn disarm_for_deliberate_exit() {
    let Some(active) = ACTIVE.get() else {
        return;
    };
    locked(|| {
        write_state_best_effort(
            &active.path,
            &RenderState::written_here(active.remembered, None),
        );
    });
}

/// Takes the marker down if `setup` does not reach its end.
///
/// There are a dozen fallible steps between the top of that closure and
/// its `Ok(())` — a tray menu item, the default window icon, the tray
/// itself — and every one of them aborts the launch before the frontend
/// can report a paint. Each would otherwise leave the marker armed, and
/// a tray icon that failed to build would send the next launch into
/// software rendering.
///
/// A guard rather than a call at each `?`, because the thirteenth one
/// someone adds would not get a call. It also covers a panic, which
/// unwinds through here.
///
/// It does **not** cover `std::process::exit`, which runs no
/// destructors: the paths that leave that way disarm explicitly.
pub struct SetupGuard {
    reached_the_end: bool,
}

impl SetupGuard {
    /// Arm the guard. Call first thing in `setup`.
    pub fn new() -> Self {
        Self {
            reached_the_end: false,
        }
    }

    /// `setup` finished. The window is coming, so the marker stays
    /// armed until something paints — which is the whole point of it.
    pub fn succeeded(mut self) {
        self.reached_the_end = true;
    }
}

impl Drop for SetupGuard {
    fn drop(&mut self) {
        if !self.reached_the_end {
            disarm_for_deliberate_exit();
        }
    }
}

/// Forget that software rendering was ever needed, so the next launch
/// tries the GPU again.
///
/// The way back for a user whose hardware was fixed, whose driver was
/// updated, or who hit the fallback once for an unrelated reason —
/// a force-quit during startup looks exactly like a GPU that cannot
/// paint, and nothing can tell them apart from here.
pub fn retry_gpu() -> std::io::Result<()> {
    let Some(active) = ACTIVE.get() else {
        return Ok(());
    };
    // The armed marker for the *current* launch stays: this launch has
    // painted or it has not, and that question is not what is being
    // answered here.
    locked(|| {
        // This instance's own marker, not whatever the file says: a
        // duplicate launch may have written its guess over it.
        let armed =
            (!PAINTED.load(std::sync::atomic::Ordering::Acquire)).then_some(active.decision.mode);
        write_state(&active.path, &RenderState::written_here(None, armed))?;
        // Inside the lock, and only once the write landed. Raised
        // before the write, a retry reported as failed would still stop
        // a later paint from restoring the fallback — quietly doing
        // what the user was told had not happened. Raised after the
        // lock, a paint slipping into that gap would read the old value
        // and write the fallback straight back.
        RETRY_REQUESTED.store(true, std::sync::atomic::Ordering::Release);
        Ok::<(), std::io::Error>(())
    })?;
    tracing::info!("renderer: forgot the software fallback; the next launch will try the GPU");
    Ok(())
}

#[cfg(test)]
mod tests {
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
}

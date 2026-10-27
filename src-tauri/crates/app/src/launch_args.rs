//! Files handed to WaveFlow on its command line: a double-click on an
//! audio file, "Open with WaveFlow", `waveflow song.flac` in a terminal.
//!
//! They come two ways. The first launch reads its own arguments. A launch
//! while WaveFlow already runs is turned away by the single-instance plugin,
//! which hands its arguments and working directory to the running one, so
//! a second double-click plays in the window already open instead of being
//! dropped. Both end in [`crate::external_playback::play_paths`].
//!
//! The Flatpak's desktop entry launches with `%U`, so files arrive there
//! as `file://` URIs; everything else passes plain paths. macOS passes no
//! arguments at all: it sends `RunEvent::Opened` with file URLs, read by
//! [`paths_from_urls`].
//!
//! Files that arrive together play together. Windows starts the program
//! once per file when several are opened at once, so each comes through the
//! single-instance plugin on its own, a moment after the last: played as
//! they came, every one would replace the queue the one before had just
//! started. They are gathered until none has arrived for a moment, then
//! played as one queue: in path order when several launches brought them,
//! since those arrive in no reliable order ([`play_order`]).
//!
//! A launch can also carry `--handoff <file>`: another player (QuickPlay)
//! handing over what it was playing ([`waveflow_core::handoff`]). That one
//! is not gathered: it names its whole queue, and plays as soon as the
//! player is up ([`take_over_when_ready`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

use crate::audio::engine::LoadIntent;
use crate::audio::AudioEngine;
use crate::state::AppState;

/// The files and folders named by `args`, the program's own path first as
/// `std::env::args_os` and the single-instance plugin both give it. Options
/// (`--minimized`, `--safe-mode`) are skipped, a relative path is taken
/// from `cwd`, and a name that does not exist is dropped.
pub fn paths_from_args<I>(args: I, cwd: Option<&Path>) -> Vec<PathBuf>
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    args.into_iter()
        .skip(1)
        .map(Into::into)
        .filter(|arg: &OsString| !arg.to_string_lossy().starts_with('-'))
        .filter_map(|arg| {
            let text = arg.to_string_lossy();
            let path = if text.starts_with("file://") {
                url::Url::parse(&text).ok()?.to_file_path().ok()?
            } else {
                PathBuf::from(arg)
            };
            Some(normalize(&match cwd {
                Some(cwd) if path.is_relative() => cwd.join(path),
                _ => path,
            }))
        })
        .filter(|path| path.exists())
        .collect()
}

/// `path` with its `.` and `..` resolved by name, the way the library stores
/// a track's path, so `waveflow ../Music/song.flac` still finds the library
/// track. Lexical on purpose: `canonicalize` would also follow links and,
/// on Windows, add a `\\?\` prefix no stored path has.
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// The local files among the URLs macOS opens the app with. Anything else
/// (a web link, a file gone in the meantime) is dropped.
#[cfg(any(target_os = "macos", test))]
pub fn paths_from_urls<'a>(urls: impl IntoIterator<Item = &'a url::Url>) -> Vec<PathBuf> {
    urls.into_iter()
        .filter_map(|url| url.to_file_path().ok())
        .filter(|path| path.exists())
        .collect()
}

/// How long the files of one open are waited for after the last one came:
/// Windows starts the program again for each, and each hands its file over
/// through the single-instance plugin a moment after the one before.
const GATHER_QUIET: Duration = Duration::from_millis(400);

/// The longest the gathering waits once the player is up, however steadily
/// files keep coming.
const GATHER_LIMIT: Duration = Duration::from_secs(3);

/// Files waiting to play, when the last of them arrived, and how many
/// launches brought them.
struct Gathering {
    paths: Vec<PathBuf>,
    last: Instant,
    arrivals: usize,
}

/// The open being gathered, if one is. `None` when nothing is waiting.
static GATHERING: Mutex<Option<Gathering>> = Mutex::new(None);

/// Add `more` to `paths`, each file once, in the order they came.
fn gather(paths: &mut Vec<PathBuf>, more: Vec<PathBuf>) {
    for path in more {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
}

/// The order a gathered open plays in. One launch keeps the order it named
/// its files in (`waveflow b.flac a.flac` plays b first). Files from several
/// launches are put in path order: the launches reach the running window in
/// no reliable order (measured: Explorer's three files arrived third,
/// second, first, the first launch's own arguments read last, after its
/// setup), so their arrival order means nothing.
fn play_order(mut paths: Vec<PathBuf>, arrivals: usize) -> Vec<PathBuf> {
    if arrivals > 1 {
        paths.sort_by_cached_key(|path| path.to_string_lossy().to_lowercase());
    }
    paths
}

/// Act on what a launch was started with: a handoff when it names one,
/// otherwise the files it names. `args` starts with the program's path.
pub fn open_when_ready<I>(app: &AppHandle, args: I, cwd: Option<&Path>)
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    match waveflow_core::handoff::file_from_args(args.iter().cloned(), cwd) {
        Some(file) => take_over_when_ready(app, file),
        None => play_when_ready(app, paths_from_args(args, cwd)),
    }
}

/// Wait until a file can play: the state and the engine exist and, within
/// a bound, the active profile is open, so a file can be told apart from a
/// library track. With no profile at all (the first run, before
/// onboarding) the files still play, as files. Returns the intent the
/// files play under, `early` when the caller claimed one already; `None`
/// when the player never came up.
///
/// The intent is claimed as soon as the engine exists (#622), not when the
/// files finally play: a track the user picks while they wait is the newer
/// choice, and the files give way to it. Nothing can be picked before the
/// engine exists, so nothing is lost by waiting for it.
async fn player_ready(app: &AppHandle, early: Option<LoadIntent>) -> Option<LoadIntent> {
    let mut intent = early;
    let mut ready = false;
    for _ in 0..150 {
        if let (Some(state), Some(engine)) = (
            app.try_state::<AppState>(),
            app.try_state::<Arc<AudioEngine>>(),
        ) {
            intent.get_or_insert_with(|| engine.next_load_intent());
            ready = true;
            if state.require_profile_pool().await.is_ok() {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Both the state and the engine, not the engine alone: playing reads
    // the state.
    intent.filter(|_| ready)
}

/// Carry on the playback handed over in `file` once the player is up, and
/// remove the file if the sender wrote it there for us.
fn take_over_when_ready(app: &AppHandle, file: PathBuf) {
    let early = app
        .try_state::<Arc<AudioEngine>>()
        .map(|engine| engine.next_load_intent());
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let read = tokio::task::spawn_blocking(move || {
            let handoff = waveflow_core::handoff::Handoff::read(&file);
            if waveflow_core::handoff::is_handoff_file(&file) {
                let _ = std::fs::remove_file(&file);
            }
            handoff
        })
        .await;
        let handoff = match read {
            Ok(Ok(handoff)) => handoff,
            Ok(Err(err)) => {
                tracing::warn!(%err, "could not read the playback handed over");
                return;
            }
            Err(err) => {
                tracing::warn!(%err, "reading the playback handed over did not finish");
                return;
            }
        };
        let Some(intent) = player_ready(&app, early).await else {
            tracing::warn!("the player never came up; not taking over the playback handed over");
            return;
        };
        if let Err(err) = crate::external_playback::take_over(&app, handoff, intent).await {
            tracing::warn!(%err, "could not take over the playback handed over");
        }
    });
}

/// Play `paths` once the active profile is open, which is when a file can
/// be told apart from a library track. At launch that is a moment after
/// `setup` returns; from a second launch it already is. Files that arrive
/// while some are already waiting join them (see the module comment).
pub fn play_when_ready(app: &AppHandle, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    {
        let mut gathering = GATHERING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(waiting) = gathering.as_mut() {
            // A task is already waiting to play; it takes these too.
            gather(&mut waiting.paths, paths);
            waiting.last = Instant::now();
            waiting.arrivals += 1;
            return;
        }
        let mut first = Vec::new();
        gather(&mut first, paths);
        *gathering = Some(Gathering {
            paths: first,
            last: Instant::now(),
            arrivals: 1,
        });
    }
    // Claimed here when the engine already exists, before the task is even
    // scheduled; at a launch, as soon as it does (below).
    let early = app
        .try_state::<Arc<AudioEngine>>()
        .map(|engine| engine.next_load_intent());
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // A second launch can arrive while this instance is still in
        // `setup`, before its state and engine exist: wait for them, then
        // for the profile.
        let intent = player_ready(&app, early).await;
        // Then until the files stop coming. The wait for the player above
        // gathers too, so a cold start opened with many files has them all.
        let started = Instant::now();
        loop {
            let since_last = GATHERING
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map_or(GATHER_QUIET, |waiting| waiting.last.elapsed());
            if since_last >= GATHER_QUIET || started.elapsed() >= GATHER_LIMIT {
                break;
            }
            tokio::time::sleep(GATHER_QUIET - since_last).await;
        }
        // Taken on every way out, so the next open starts a gathering of
        // its own.
        let paths = GATHERING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .map(|waiting| play_order(waiting.paths, waiting.arrivals))
            .unwrap_or_default();
        let Some(intent) = intent else {
            tracing::warn!(
                "the player never came up; not playing the files WaveFlow was opened with"
            );
            return;
        };
        if let Err(err) =
            crate::external_playback::play_paths_with_intent(&app, paths, intent).await
        {
            tracing::warn!(%err, "could not play the files WaveFlow was opened with");
        }
    });
}

#[cfg(test)]
mod tests;

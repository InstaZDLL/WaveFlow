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
//! as `file://` URIs; everything else passes plain paths.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Manager};

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

/// Play `paths` once the active profile is open, which is when a file can
/// be told apart from a library track. At launch that is a moment after
/// `setup` returns; from a second launch it already is.
pub fn play_when_ready(app: &AppHandle, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // A second launch can arrive while this instance is still in
        // `setup`, before its state and engine exist: wait for them, then
        // for the profile. Bounded: with no profile at all (the first run,
        // before onboarding) the files still play, as files.
        let mut ready = false;
        for _ in 0..150 {
            if let (Some(state), Some(_)) = (
                app.try_state::<AppState>(),
                app.try_state::<Arc<AudioEngine>>(),
            ) {
                ready = true;
                if state.require_profile_pool().await.is_ok() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if !ready {
            tracing::warn!(
                "the player never came up; not playing the files WaveFlow was opened with"
            );
            return;
        }
        if let Err(err) = crate::external_playback::play_paths(&app, paths).await {
            tracing::warn!(%err, "could not play the files WaveFlow was opened with");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_and_its_options_are_not_files() {
        let dir = tempfile::tempdir().unwrap();
        let song = dir.path().join("song.flac");
        std::fs::write(&song, b"x").unwrap();
        let args = [
            OsString::from("waveflow"),
            OsString::from("--minimized"),
            song.clone().into_os_string(),
        ];
        assert_eq!(paths_from_args(args, None), [song]);
    }

    #[test]
    fn a_relative_path_is_taken_from_the_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("song.flac"), b"x").unwrap();
        let args = ["waveflow", "song.flac"];
        assert_eq!(
            paths_from_args(args, Some(dir.path())),
            [dir.path().join("song.flac")]
        );
    }

    #[test]
    fn dots_in_a_path_are_resolved_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("Music");
        std::fs::create_dir(&music).unwrap();
        std::fs::write(music.join("song.flac"), b"x").unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let args = ["waveflow", "../Music/./song.flac"];
        assert_eq!(
            paths_from_args(args, Some(&elsewhere)),
            [music.join("song.flac")]
        );
    }

    #[test]
    fn a_file_uri_is_a_path_and_a_missing_file_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let song = dir.path().join("my song.flac");
        std::fs::write(&song, b"x").unwrap();
        let uri = url::Url::from_file_path(&song).unwrap().to_string();
        let gone = dir.path().join("gone.flac").display().to_string();
        let args = vec!["waveflow".to_string(), uri, gone];
        assert_eq!(paths_from_args(args, None), [song]);
    }
}

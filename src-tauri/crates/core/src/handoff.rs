//! Handing playback over to WaveFlow from another player.
//!
//! QuickPlay plays a file the moment it is opened, without a library. When
//! its user wants the rest of WaveFlow, it hands over what it is playing —
//! the queue, the track, the position, whether it was playing — and stops:
//! WaveFlow carries on from the same spot.
//!
//! The sender writes a [`Handoff`] to a file of its own ([`Handoff::write`])
//! and starts `waveflow --handoff <file>` ([`Handoff::args`]). A file, not
//! the JSON itself on the command line: Windows caps a command line at
//! 32 767 characters, which a queue of a few hundred paths already
//! exceeds. A WaveFlow already running receives the arguments through its
//! single-instance plugin, so either way it ends up reading the file
//! ([`Handoff::read`]) and removing it ([`is_handoff_file`]).
//!
//! The format is versioned. A field added later is ignored by an older
//! WaveFlow, so adding one keeps [`VERSION`]; a change an older WaveFlow
//! would misread raises it, and that WaveFlow then refuses the handoff
//! rather than playing the wrong thing.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The command-line option that names a handoff file.
pub const FLAG: &str = "--handoff";

/// The newest format this build reads and the one it writes.
pub const VERSION: u32 = 1;

/// How a handoff file's name starts. With [`EXTENSION`] and the temporary
/// directory, it is how the receiver knows a file is one it may remove.
const FILE_PREFIX: &str = "waveflow-handoff-";
const EXTENSION: &str = "json";

/// The largest handoff file read: a queue of [`MAX_PATHS`] long paths
/// stays well under it.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The most paths a handoff carries; the rest are dropped.
pub const MAX_PATHS: usize = 5_000;

/// What another player hands over: its queue and where it was in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Handoff {
    /// The format, [`VERSION`] when written by this build.
    pub version: u32,
    /// The queue, in play order: audio files, by absolute path.
    pub paths: Vec<PathBuf>,
    /// The entry of `paths` that was playing.
    pub index: usize,
    /// How far into it, in milliseconds.
    pub position_ms: u64,
    /// Whether it was playing, or paused there.
    pub playing: bool,
}

/// Why a handoff could not be read.
#[derive(Debug, thiserror::Error)]
pub enum HandoffError {
    #[error("reading the handoff file: {0}")]
    Io(#[from] std::io::Error),
    #[error("the handoff file is larger than {MAX_BYTES} bytes")]
    TooLarge,
    #[error("the handoff file is not valid: {0}")]
    Invalid(#[from] serde_json::Error),
    #[error("the handoff is format {0}, newer than this WaveFlow reads ({VERSION})")]
    Newer(u32),
}

impl Handoff {
    /// A handoff in the current format.
    pub fn new(paths: Vec<PathBuf>, index: usize, position_ms: u64, playing: bool) -> Self {
        Self {
            version: VERSION,
            paths,
            index,
            position_ms,
            playing,
        }
    }

    /// Write the handoff to a new file in the temporary directory and
    /// return its path, for [`Self::args`]. The receiver removes it.
    pub fn write(&self) -> std::io::Result<PathBuf> {
        let json = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        let dir = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        for attempt in 0u32.. {
            let path = dir.join(format!(
                "{FILE_PREFIX}{}-{stamp}-{attempt}.{EXTENSION}",
                std::process::id()
            ));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            // The queue names the user's files, and `/tmp` is shared with
            // every other account on the machine.
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(mut file) => {
                    file.write_all(&json)?;
                    return Ok(path);
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && attempt < 16 => {}
                Err(err) => return Err(err),
            }
        }
        unreachable!("the loop returns on its last attempt")
    }

    /// The arguments that hand `file` to WaveFlow, after the program's path.
    pub fn args(file: &Path) -> [OsString; 2] {
        [OsString::from(FLAG), file.as_os_str().to_owned()]
    }

    /// Read a handoff file. A newer format than [`VERSION`] is refused, and
    /// a queue longer than [`MAX_PATHS`] is cut there.
    pub fn read(file: &Path) -> Result<Self, HandoffError> {
        use std::io::Read;
        // One handle for the checks and the read, and the read itself
        // bounded: a path swapped between a check and a second open could
        // otherwise be read without limit.
        let opened = std::fs::File::open(file)?;
        let meta = opened.metadata()?;
        if !meta.is_file() {
            return Err(HandoffError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a regular file",
            )));
        }
        if meta.len() > MAX_BYTES {
            return Err(HandoffError::TooLarge);
        }
        let mut bytes = Vec::new();
        opened.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(HandoffError::TooLarge);
        }
        let mut handoff: Self = serde_json::from_slice(&bytes)?;
        if handoff.version > VERSION {
            return Err(HandoffError::Newer(handoff.version));
        }
        handoff.paths.truncate(MAX_PATHS);
        Ok(handoff)
    }
}

/// The handoff file `args` name, if they name one: the argument after
/// [`FLAG`], taken from `cwd` when relative. `args` starts with the
/// program's own path, as `std::env::args_os` gives it.
pub fn file_from_args<I>(args: I, cwd: Option<&Path>) -> Option<PathBuf>
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    let mut args = args.into_iter().skip(1).map(Into::into);
    args.find(|arg| arg == FLAG)?;
    let file = PathBuf::from(args.next()?);
    Some(match cwd {
        Some(cwd) if file.is_relative() => cwd.join(file),
        _ => file,
    })
}

/// Whether `file` is one [`Handoff::write`] made, so the receiver may
/// remove it once read. Anything else named after [`FLAG`] is left where it
/// is: the option must not become a way to delete a file.
pub fn is_handoff_file(file: &Path) -> bool {
    let named = file
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(FILE_PREFIX))
        && file.extension().and_then(|e| e.to_str()) == Some(EXTENSION);
    named && file.parent() == Some(std::env::temp_dir().as_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handoff_survives_the_trip_through_its_file() {
        let handoff = Handoff::new(
            vec![PathBuf::from("/m/a.flac"), PathBuf::from("/m/b.flac")],
            1,
            42_500,
            false,
        );
        let file = handoff.write().unwrap();
        assert!(is_handoff_file(&file));
        let read = Handoff::read(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        assert_eq!(read, handoff);
    }

    #[test]
    fn the_file_is_found_after_the_flag_and_taken_from_the_working_directory() {
        let cwd = Path::new("/home/me");
        let args = ["waveflow", "--minimized", FLAG, "h.json"];
        assert_eq!(file_from_args(args, Some(cwd)), Some(cwd.join("h.json")));
        assert_eq!(file_from_args(["waveflow", "song.flac"], Some(cwd)), None);
        // The flag with nothing after it names no file.
        assert_eq!(file_from_args(["waveflow", FLAG], Some(cwd)), None);
    }

    #[test]
    fn only_a_file_the_sender_wrote_may_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        // The right name, in the wrong folder.
        assert!(!is_handoff_file(
            &dir.path().join("waveflow-handoff-1.json")
        ));
        // The right folder, a file of someone else's.
        assert!(!is_handoff_file(&std::env::temp_dir().join("notes.json")));
        assert!(!is_handoff_file(
            &std::env::temp_dir().join("waveflow-handoff-1.txt")
        ));
    }

    #[test]
    fn a_newer_format_is_refused_and_an_added_field_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("h.json");
        std::fs::write(
            &file,
            r#"{"version":1,"paths":["/m/a.flac"],"index":0,"position_ms":0,"playing":true,"volume":0.5}"#,
        )
        .unwrap();
        assert!(Handoff::read(&file).is_ok());
        std::fs::write(
            &file,
            r#"{"version":2,"paths":["/m/a.flac"],"index":0,"position_ms":0,"playing":true}"#,
        )
        .unwrap();
        assert!(matches!(Handoff::read(&file), Err(HandoffError::Newer(2))));
    }

    #[test]
    fn a_long_queue_is_cut() {
        let paths = (0..MAX_PATHS + 10)
            .map(|i| PathBuf::from(format!("/m/{i}.flac")))
            .collect();
        let file = Handoff::new(paths, 0, 0, true).write().unwrap();
        let read = Handoff::read(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        assert_eq!(read.paths.len(), MAX_PATHS);
    }
}

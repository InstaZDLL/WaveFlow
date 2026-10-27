//! Adding files dropped on the window to the library.
//!
//! A drop is a handful of files or an album folder, not a place to watch:
//! registering where they lie made the folder they came from a library
//! folder, and dropping one file from `Music` added the whole of `Music`,
//! thousands of tracks the user never asked for. So the dropped files are
//! **copied** into one folder WaveFlow manages, `WaveFlow` under the user's
//! music folder, and that folder is the library folder. The originals stay
//! where they were.
//!
//! - **As they are.** A dropped folder keeps its name and its tree under
//!   the managed folder; a dropped file goes to its root. Nothing is
//!   renamed from its tags. A folder brings its audio files and what the
//!   app reads beside them (cover images, lyrics); a file brings its
//!   same-name lyrics.
//! - **Lyrics follow their audio.** A `.lrc` or `.ttml` beside an audio file
//!   of the same name is copied under the name the audio file ends up
//!   with, so a numbered copy keeps its lyrics.
//! - **Already in the library is passed over.** An item under a library
//!   folder, the managed one included, has nothing to copy. A folder that
//!   holds a library folder is refused: copying it would duplicate that
//!   folder's tracks, and dropping the music folder that holds the managed
//!   one would copy it into itself.
//! - **A file already copied is not copied again.** One at the target with
//!   the same name and size is taken as this file; one with the same name
//!   and another size gets a numbered name.
//! - A copy is written under a temporary name and renamed when complete,
//!   so neither the watcher nor a scan ever indexes half a file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use waveflow_core::scanner::extract::FOLDER_COVER_EXTENSIONS;
use waveflow_core::scanner::folders::{overlap, FolderRelation};

use crate::commands::scan::ScanSummary;

/// The managed folder's name under the user's music folder.
pub const MANAGED_FOLDER_NAME: &str = "WaveFlow";

/// Lyrics sidecars, copied beside the audio file of the same name. Cover
/// images go too, by the scanner's own list ([`FOLDER_COVER_EXTENSIONS`]),
/// so the two cannot drift.
const LYRICS_EXTENSIONS: &[&str] = &["lrc", "ttml"];

/// Most files one drop copies. Past it the drop is cut short and says so.
const MAX_FILES: usize = 20_000;

/// Most entries one drop walks, as [`crate::external_playback::expand`]
/// bounds its own: a drive dropped by mistake is not walked to the end.
const MAX_VISITED: usize = 200_000;

/// How often the walk asks whether to stop, in entries.
const STOP_CHECK_EVERY: usize = 256;

/// What a drop did, for the summary the frontend shows.
#[derive(Debug, Default, Serialize)]
pub struct DropImportSummary {
    /// The managed folder the files were copied into.
    pub folder: String,
    /// Files copied.
    pub copied: u32,
    /// Files already in the managed folder from an earlier drop.
    pub already_copied: u32,
    /// Dropped items already under a library folder, passed over.
    pub in_library: u32,
    /// Dropped folders holding a library folder, refused.
    pub refused: u32,
    /// Entries that could not be read, files that could not be copied.
    pub errors: u32,
    /// The drop held more than [`MAX_FILES`] files; the rest were left.
    pub truncated: bool,
    /// The user stopped the copy.
    pub cancelled: bool,
    /// The managed folder's scan, when anything was copied.
    pub scan: Option<ScanSummary>,
}

/// `WaveFlow` under the user's music folder (`~/Music` where the platform
/// names none).
pub fn managed_folder() -> Option<PathBuf> {
    dirs::audio_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Music")))
        .map(|music| music.join(MANAGED_FOLDER_NAME))
}

/// One file to copy, where, and the lyrics that follow it.
#[derive(Debug, PartialEq, Eq)]
pub struct Copy {
    pub from: PathBuf,
    pub to: PathBuf,
    /// Lyrics sidecars of an audio file, copied beside wherever it lands.
    pub lyrics: Vec<PathBuf>,
}

/// The copies a drop makes, and what it passes over.
#[derive(Debug, Default)]
pub struct Plan {
    pub copies: Vec<Copy>,
    pub in_library: u32,
    pub refused: u32,
    /// Entries the walk could not read: a folder it was refused, a file
    /// gone in the meantime. Reported, so a partial import does not read as
    /// a complete one.
    pub read_errors: u32,
    pub truncated: bool,
    /// `should_stop` said so before the walk was done.
    pub stopped: bool,
}

/// What to copy for `dropped` into `managed`, given the library folders
/// (the managed folder counts as one). Blocking: walks dropped folders,
/// asking `should_stop` as it goes.
pub fn plan(
    dropped: &[PathBuf],
    managed: &Path,
    library: &[PathBuf],
    should_stop: &dyn Fn() -> bool,
) -> Plan {
    let mut out = Plan::default();
    let mut visited = 0usize;
    let folders = || library.iter().map(PathBuf::as_path).chain([managed]);
    for item in dropped {
        if should_stop() {
            out.stopped = true;
            return out;
        }
        match overlap(item, folders()) {
            Some((FolderRelation::Same | FolderRelation::Inside, _)) => {
                out.in_library += 1;
                continue;
            }
            Some((FolderRelation::Contains, _)) => {
                out.refused += 1;
                continue;
            }
            None => {}
        }
        if item.is_dir() {
            if !plan_folder(item, managed, &mut out, &mut visited, should_stop) {
                return out;
            }
        } else if item.is_file() && is_audio(item) {
            if out.copies.len() >= MAX_FILES {
                out.truncated = true;
                return out;
            }
            let Some(name) = item.file_name() else {
                continue;
            };
            out.copies.push(Copy {
                from: item.clone(),
                to: managed.join(name),
                lyrics: LYRICS_EXTENSIONS
                    .iter()
                    .map(|ext| item.with_extension(ext))
                    .filter(|lyrics| lyrics.is_file())
                    .collect(),
            });
        }
    }
    out
}

/// Add a dropped folder's files to `out`. `false` when the walk stopped
/// early (a bound, or `should_stop`), which ends the plan.
fn plan_folder(
    folder: &Path,
    managed: &Path,
    out: &mut Plan,
    visited: &mut usize,
    should_stop: &dyn Fn() -> bool,
) -> bool {
    let base = managed.join(folder.file_name().unwrap_or_else(|| "Import".as_ref()));
    let walk = walkdir::WalkDir::new(folder)
        .sort_by(|a, b| a.file_name().cmp(b.file_name()))
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_dir()
                || !waveflow_core::scanner::is_reserved_dir_name(entry.file_name())
        });
    // Each audio file by its folder and lower-cased stem, so the lyrics
    // found after it in the walk can join it.
    let mut audio_at: HashMap<(PathBuf, String), usize> = HashMap::new();
    let mut lyrics: Vec<PathBuf> = Vec::new();
    for entry in walk {
        *visited += 1;
        if *visited % STOP_CHECK_EVERY == 0 && should_stop() {
            out.stopped = true;
            return false;
        }
        if *visited >= MAX_VISITED || out.copies.len() >= MAX_FILES {
            out.truncated = true;
            return false;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(%err, "drop import: could not read an entry");
                out.read_errors += 1;
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if has_extension(path, LYRICS_EXTENSIONS) {
            lyrics.push(entry.into_path());
            continue;
        }
        let audio = is_audio(path);
        if !audio && !has_extension(path, FOLDER_COVER_EXTENSIONS) {
            continue;
        }
        let Ok(rel) = path.strip_prefix(folder) else {
            continue;
        };
        if audio {
            if let Some(key) = stem_key(path) {
                audio_at.insert(key, out.copies.len());
            }
        }
        out.copies.push(Copy {
            to: base.join(rel),
            from: entry.into_path(),
            lyrics: Vec::new(),
        });
    }
    // Lyrics beside an audio file of the same name follow it; any other is
    // copied where it was.
    for path in lyrics {
        match stem_key(&path).and_then(|key| audio_at.get(&key)) {
            Some(&index) => out.copies[index].lyrics.push(path),
            None => {
                let Ok(rel) = path.strip_prefix(folder) else {
                    continue;
                };
                out.copies.push(Copy {
                    to: base.join(rel),
                    from: path,
                    lyrics: Vec::new(),
                });
            }
        }
    }
    true
}

/// A file's folder and lower-cased stem: the key that pairs lyrics with
/// their audio file.
fn stem_key(path: &Path) -> Option<(PathBuf, String)> {
    let parent = path.parent()?.to_path_buf();
    let stem = path.file_stem()?.to_string_lossy().to_lowercase();
    Some((parent, stem))
}

fn is_audio(path: &Path) -> bool {
    waveflow_core::scanner::extract::is_scannable_audio(path)
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| extensions.iter().any(|e| ext.eq_ignore_ascii_case(e)))
}

/// Held across one planned copy, from choosing its target name to the
/// rename that completes it. Two drops running at once would otherwise both
/// find a name free, write the same `.waveflow-part`, and the second rename
/// would replace the first file. Copies are one at a time within a drop
/// anyway, so this only makes two drops take turns, file by file.
static COPY_LOCK: Mutex<()> = Mutex::new(());

/// Make one planned copy, its lyrics after it, and count the outcome.
pub fn apply(copy: &Copy, summary: &mut DropImportSummary) {
    // A copy that panicked holding the lock left nothing half-reserved: its
    // target is either renamed in place or still a `.waveflow-part`. So the
    // lock is taken back rather than the import abandoned.
    let _turn = COPY_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let landed = match copy_one(&copy.from, &copy.to) {
        Ok(Copied::New(at)) => {
            summary.copied += 1;
            at
        }
        Ok(Copied::AlreadyThere(at)) => {
            summary.already_copied += 1;
            at
        }
        Err(err) => {
            tracing::warn!(from = %copy.from.display(), %err, "drop import: copy failed");
            summary.errors += 1;
            return;
        }
    };
    for lyrics in &copy.lyrics {
        let Some(ext) = lyrics.extension() else {
            continue;
        };
        match copy_beside(lyrics, &landed.with_extension(ext)) {
            Ok(Copied::New(_)) => summary.copied += 1,
            Ok(Copied::AlreadyThere(_)) => summary.already_copied += 1,
            Err(err) => {
                tracing::warn!(from = %lyrics.display(), %err, "drop import: copy failed");
                summary.errors += 1;
            }
        }
    }
}

/// What one copy did.
#[derive(Debug, PartialEq, Eq)]
pub enum Copied {
    /// Written, at this path (a numbered name when the first was taken).
    New(PathBuf),
    /// A file of the same name and size was already there, at this path.
    AlreadyThere(PathBuf),
}

/// Copy `from` to `to`, or to `to` numbered when another file holds that
/// name, through a temporary name renamed when complete.
pub fn copy_one(from: &Path, to: &Path) -> std::io::Result<Copied> {
    let size = std::fs::metadata(from)?.len();
    let mut target = to.to_path_buf();
    let mut n = 1u32;
    loop {
        match std::fs::metadata(&target) {
            Ok(meta) if meta.is_file() && meta.len() == size => {
                return Ok(Copied::AlreadyThere(target))
            }
            Ok(_) => {
                n += 1;
                if n > 99 {
                    return Err(std::io::Error::other("no free name at the target"));
                }
                target = numbered(to, n);
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
            Err(err) => return Err(err),
        }
    }
    write_through_part(from, &target)?;
    Ok(Copied::New(target))
}

/// Copy a sidecar to exactly `to`, beside the file it belongs to. One
/// already there is kept, whatever it holds: it may be lyrics the user
/// edited, and a numbered copy would belong to no audio file.
fn copy_beside(from: &Path, to: &Path) -> std::io::Result<Copied> {
    if to.exists() {
        return Ok(Copied::AlreadyThere(to.to_path_buf()));
    }
    write_through_part(from, to)?;
    Ok(Copied::New(to.to_path_buf()))
}

/// Write `from` to `target` under a temporary name, renamed when complete.
fn write_through_part(from: &Path, target: &Path) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut part_name = target.file_name().unwrap_or_default().to_os_string();
    part_name.push(".waveflow-part");
    let part = target.with_file_name(part_name);
    if let Err(err) = std::fs::copy(from, &part).and_then(|_| std::fs::rename(&part, target)) {
        let _ = std::fs::remove_file(&part);
        return Err(err);
    }
    Ok(())
}

/// `name (n).ext` beside `path`.
fn numbered(path: &Path, n: u32) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let name = match path.extension() {
        Some(ext) => format!("{stem} ({n}).{}", ext.to_string_lossy()),
        None => format!("{stem} ({n})"),
    };
    path.with_file_name(name)
}

#[cfg(test)]
mod tests;

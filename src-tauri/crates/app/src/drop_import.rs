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
//!   same-name `.lrc`.
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

use std::path::{Path, PathBuf};

use serde::Serialize;
use waveflow_core::scanner::folders::{relation, FolderRelation};

use crate::commands::scan::ScanSummary;

/// The managed folder's name under the user's music folder.
pub const MANAGED_FOLDER_NAME: &str = "WaveFlow";

/// Files beside the audio that a dropped folder brings along: the app reads
/// folder covers and lyrics sidecars from them.
const SIDECAR_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "lrc", "ttml"];

/// Most files one drop copies. Past it the drop is cut short and says so.
const MAX_FILES: usize = 20_000;

/// Most entries one drop walks, as [`crate::external_playback::expand`]
/// bounds its own: a drive dropped by mistake is not walked to the end.
const MAX_VISITED: usize = 200_000;

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
    /// Files that could not be read or written.
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

/// The library folder that will index the managed folder's files: the
/// managed folder's own row, else the deepest library folder holding it (a
/// user whose library is the whole music folder already scans it). `None`
/// when neither exists and the managed folder must be added.
///
/// A library folder *inside* the managed folder is an overlap the drop
/// cannot resolve, and is returned as one.
pub fn indexing_folder(
    managed: &Path,
    folders: &[(i64, PathBuf)],
) -> Result<Option<i64>, (FolderRelation, PathBuf)> {
    let mut best: Option<(i64, usize)> = None;
    for (id, path) in folders {
        match relation(managed, path) {
            Some(FolderRelation::Same) => return Ok(Some(*id)),
            Some(FolderRelation::Inside) => {
                let depth = path.components().count();
                // Spelled without `Option::is_none_or`: the MSRV is 1.80.
                let deeper = match best {
                    Some((_, deepest)) => depth > deepest,
                    None => true,
                };
                if deeper {
                    best = Some((*id, depth));
                }
            }
            Some(FolderRelation::Contains) => {
                return Err((FolderRelation::Contains, path.clone()));
            }
            None => {}
        }
    }
    Ok(best.map(|(id, _)| id))
}

/// The library folder `candidate` overlaps, if any, and how: the same folder
/// first, then one holding it, then one under it.
pub fn overlap<'a>(
    candidate: &Path,
    folders: impl IntoIterator<Item = &'a Path>,
) -> Option<(FolderRelation, PathBuf)> {
    let mut found: Option<(FolderRelation, PathBuf)> = None;
    for folder in folders {
        let Some(rel) = relation(candidate, folder) else {
            continue;
        };
        let rank = |r: FolderRelation| match r {
            FolderRelation::Same => 0,
            FolderRelation::Inside => 1,
            FolderRelation::Contains => 2,
        };
        let closer = match &found {
            Some((kept, _)) => rank(rel) < rank(*kept),
            None => true,
        };
        if closer {
            found = Some((rel, folder.to_path_buf()));
        }
    }
    found
}

/// One file to copy, and where.
#[derive(Debug, PartialEq, Eq)]
pub struct Copy {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// The copies a drop makes, and what it passes over.
#[derive(Debug, Default)]
pub struct Plan {
    pub copies: Vec<Copy>,
    pub in_library: u32,
    pub refused: u32,
    pub truncated: bool,
}

/// What to copy for `dropped` into `managed`, given the library folders
/// (the managed folder counts as one). Blocking: walks dropped folders.
pub fn plan(dropped: &[PathBuf], managed: &Path, library: &[PathBuf]) -> Plan {
    let mut out = Plan::default();
    let mut visited = 0usize;
    let folders = || library.iter().map(PathBuf::as_path).chain([managed]);
    for item in dropped {
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
            let base = managed.join(item.file_name().unwrap_or_else(|| "Import".as_ref()));
            let walk = walkdir::WalkDir::new(item)
                .sort_by(|a, b| a.file_name().cmp(b.file_name()))
                .into_iter()
                .filter_entry(|entry| {
                    entry.depth() == 0
                        || !entry.file_type().is_dir()
                        || !waveflow_core::scanner::is_reserved_dir_name(entry.file_name())
                });
            for entry in walk.flatten() {
                visited += 1;
                if visited >= MAX_VISITED || out.copies.len() >= MAX_FILES {
                    out.truncated = true;
                    return out;
                }
                if !entry.file_type().is_file()
                    || !(is_audio(entry.path()) || is_sidecar(entry.path()))
                {
                    continue;
                }
                let Ok(rel) = entry.path().strip_prefix(item) else {
                    continue;
                };
                out.copies.push(Copy {
                    to: base.join(rel),
                    from: entry.into_path(),
                });
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
            });
            let lyrics = item.with_extension("lrc");
            if lyrics.is_file() {
                if let Some(name) = lyrics.file_name() {
                    out.copies.push(Copy {
                        to: managed.join(name),
                        from: lyrics,
                    });
                }
            }
        }
    }
    out
}

fn is_audio(path: &Path) -> bool {
    waveflow_core::scanner::extract::is_scannable_audio(path)
}

fn is_sidecar(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            SIDECAR_EXTENSIONS
                .iter()
                .any(|s| ext.eq_ignore_ascii_case(s))
        })
}

/// What one copy did.
#[derive(Debug, PartialEq, Eq)]
pub enum Copied {
    /// Written, at this path (a numbered name when the first was taken).
    New(PathBuf),
    /// A file of the same name and size was already there.
    AlreadyThere,
}

/// Copy `from` to `to`, or to `to` numbered when another file holds that
/// name, through a temporary name renamed when complete.
pub fn copy_one(from: &Path, to: &Path) -> std::io::Result<Copied> {
    let size = std::fs::metadata(from)?.len();
    let mut target = to.to_path_buf();
    let mut n = 1u32;
    loop {
        match std::fs::metadata(&target) {
            Ok(meta) if meta.is_file() && meta.len() == size => return Ok(Copied::AlreadyThere),
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
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut part_name = target.file_name().unwrap_or_default().to_os_string();
    part_name.push(".waveflow-part");
    let part = target.with_file_name(part_name);
    if let Err(err) = std::fs::copy(from, &part).and_then(|_| std::fs::rename(&part, &target)) {
        let _ = std::fs::remove_file(&part);
        return Err(err);
    }
    Ok(Copied::New(target))
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
mod tests {
    use super::*;

    fn touch(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn a_folder_keeps_its_tree_and_brings_covers_and_lyrics() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("Downloads").join("Album");
        touch(&album.join("01.flac"), b"a");
        touch(&album.join("CD2").join("01.mp3"), b"b");
        touch(&album.join("cover.jpg"), b"c");
        touch(&album.join("01.lrc"), b"d");
        touch(&album.join("notes.pdf"), b"e");
        touch(&album.join(".waveflow").join("clip.mp4"), b"f");
        let managed = dir.path().join("Music").join("WaveFlow");

        let plan = plan(std::slice::from_ref(&album), &managed, &[]);
        let mut to: Vec<_> = plan.copies.iter().map(|c| c.to.clone()).collect();
        to.sort();
        let base = managed.join("Album");
        assert_eq!(
            to,
            vec![
                base.join("01.flac"),
                base.join("01.lrc"),
                base.join("CD2").join("01.mp3"),
                base.join("cover.jpg"),
            ]
        );
    }

    #[test]
    fn a_file_goes_to_the_root_with_its_lyrics() {
        let dir = tempfile::tempdir().unwrap();
        let song = dir.path().join("Downloads").join("song.mp3");
        touch(&song, b"a");
        touch(&song.with_extension("lrc"), b"b");
        touch(&dir.path().join("Downloads").join("other.mp3"), b"c");
        let managed = dir.path().join("Music").join("WaveFlow");

        let plan = plan(std::slice::from_ref(&song), &managed, &[]);
        let to: Vec<_> = plan.copies.iter().map(|c| c.to.clone()).collect();
        assert_eq!(to, vec![managed.join("song.mp3"), managed.join("song.lrc")]);
    }

    #[test]
    fn library_items_are_passed_over_and_folders_holding_one_refused() {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("Music");
        let library = music.join("Albums");
        touch(&library.join("Album").join("01.flac"), b"a");
        touch(&music.join("Loose").join("02.flac"), b"b");
        let managed = music.join("WaveFlow");

        // The library's own album, the music folder that holds both the
        // library folder and the managed one, and a file already copied.
        touch(&managed.join("03.flac"), b"c");
        let dropped = [
            library.join("Album"),
            music.clone(),
            managed.join("03.flac"),
        ];
        let plan = plan(&dropped, &managed, std::slice::from_ref(&library));
        assert!(plan.copies.is_empty());
        assert_eq!(plan.in_library, 2);
        assert_eq!(plan.refused, 1);
    }

    #[test]
    fn the_managed_folder_is_indexed_by_its_row_or_the_folder_holding_it() {
        let managed = PathBuf::from("/m/Music/WaveFlow");
        let music = (1, PathBuf::from("/m/Music"));
        let own = (2, managed.clone());
        let other = (3, PathBuf::from("/m/Downloads"));
        assert_eq!(
            indexing_folder(&managed, std::slice::from_ref(&other)),
            Ok(None)
        );
        assert_eq!(
            indexing_folder(&managed, &[music.clone(), other.clone()]),
            Ok(Some(1))
        );
        assert_eq!(indexing_folder(&managed, &[music, own]), Ok(Some(2)));
        let under = (4, managed.join("Album"));
        assert_eq!(
            indexing_folder(&managed, std::slice::from_ref(&under)),
            Err((FolderRelation::Contains, under.1))
        );
    }

    #[test]
    fn a_copy_is_not_repeated_and_a_different_file_gets_a_number() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("in").join("song.mp3");
        touch(&from, b"abc");
        let to = dir.path().join("out").join("song.mp3");

        assert_eq!(copy_one(&from, &to).unwrap(), Copied::New(to.clone()));
        assert_eq!(copy_one(&from, &to).unwrap(), Copied::AlreadyThere);

        let other = dir.path().join("in2").join("song.mp3");
        touch(&other, b"abcd");
        let numbered = dir.path().join("out").join("song (2).mp3");
        assert_eq!(
            copy_one(&other, &to).unwrap(),
            Copied::New(numbered.clone())
        );
        assert_eq!(std::fs::read(numbered).unwrap(), b"abcd");
        // Nothing half-written is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("out"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".waveflow-part"))
            .collect();
        assert!(leftovers.is_empty());
    }
}

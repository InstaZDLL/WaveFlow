//! How two library folders sit relative to each other.
//!
//! A library folder owns the tracks its scan finds, and removing it deletes
//! them (`track.folder_id`). Two folders where one holds the other would
//! both claim the files they share: each scan reads them again, the last
//! one to run takes them, and removing either can take tracks the other
//! still shows. So a folder that overlaps one already there is refused,
//! and the drop import copies nothing out of a library folder.
//!
//! The comparison is by name, as the library stores paths: `.` and `..` are
//! resolved, a trailing separator means nothing, and on the platforms whose
//! file systems ignore case by default (Windows, macOS) so does this. Links
//! are not followed, which `canonicalize` would do, adding a `\\?\` prefix
//! on Windows that no stored path has.

use std::path::{Component, Path, PathBuf};

/// Where a candidate folder lies relative to a library folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderRelation {
    /// The same folder.
    Same,
    /// Under the library folder.
    Inside,
    /// Above the library folder: the library folder is under it.
    Contains,
}

/// How `candidate` lies relative to `folder`, or `None` when neither holds
/// the other.
pub fn relation(candidate: &Path, folder: &Path) -> Option<FolderRelation> {
    let candidate = parts(candidate);
    let folder = parts(folder);
    if candidate.is_empty() || folder.is_empty() {
        return None;
    }
    if candidate == folder {
        Some(FolderRelation::Same)
    } else if candidate.starts_with(&folder) {
        Some(FolderRelation::Inside)
    } else if folder.starts_with(&candidate) {
        Some(FolderRelation::Contains)
    } else {
        None
    }
}

/// The library folder `candidate` overlaps, if any, and how. When it meets
/// several, the same folder wins, then one holding it, then one under it:
/// the first two say the candidate is already covered, which is the more
/// useful thing to tell.
pub fn overlap<'a>(
    candidate: &Path,
    folders: impl IntoIterator<Item = &'a Path>,
) -> Option<(FolderRelation, PathBuf)> {
    let rank = |r: FolderRelation| match r {
        FolderRelation::Same => 0,
        FolderRelation::Inside => 1,
        FolderRelation::Contains => 2,
    };
    let mut found: Option<(FolderRelation, PathBuf)> = None;
    for folder in folders {
        let Some(rel) = relation(candidate, folder) else {
            continue;
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

/// The library folder that indexes `managed`'s files: its own row, else the
/// deepest library folder holding it (a library that is the whole music
/// folder already scans it). `None` when neither exists and `managed` must
/// be added.
///
/// A library folder *inside* `managed` is an overlap this cannot resolve,
/// and is returned as one.
pub fn indexing_folder(
    managed: &Path,
    folders: &[(i64, PathBuf)],
) -> Result<Option<i64>, (FolderRelation, PathBuf)> {
    let mut best: Option<(i64, usize)> = None;
    for (id, path) in folders {
        match relation(managed, path) {
            Some(FolderRelation::Same) => return Ok(Some(*id)),
            Some(FolderRelation::Inside) => {
                let depth = parts(path).len();
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

/// The path's components by name, `.` and `..` resolved, folded to lower
/// case where the file system ignores it.
fn parts(path: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => {
                let name = other.as_os_str().to_string_lossy();
                out.push(if cfg!(any(windows, target_os = "macos")) {
                    name.to_lowercase()
                } else {
                    name.into_owned()
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn same_inside_contains_and_apart() {
        let music = p("/home/u/Music");
        assert_eq!(relation(&music, &music), Some(FolderRelation::Same));
        assert_eq!(
            relation(&p("/home/u/Music/Albums"), &music),
            Some(FolderRelation::Inside)
        );
        assert_eq!(
            relation(&music, &p("/home/u/Music/Albums")),
            Some(FolderRelation::Contains)
        );
        assert_eq!(relation(&p("/home/u/Downloads"), &music), None);
    }

    #[test]
    fn a_shared_prefix_of_a_name_is_not_containment() {
        // `Music2` starts with the characters of `Music` but is not under it.
        assert_eq!(relation(&p("/home/u/Music2"), &p("/home/u/Music")), None);
    }

    #[test]
    fn dots_and_a_trailing_separator_change_nothing() {
        let music = p("/home/u/Music");
        assert_eq!(
            relation(&p("/home/u/Music/"), &music),
            Some(FolderRelation::Same)
        );
        assert_eq!(
            relation(&p("/home/u/Downloads/../Music/./Albums"), &music),
            Some(FolderRelation::Inside)
        );
    }

    #[test]
    fn overlap_prefers_same_then_inside_then_contains() {
        let candidate = p("/m/Music/Albums");
        let under = p("/m/Music/Albums/Live");
        let above = p("/m/Music");
        let same = p("/m/Music/Albums");
        let apart = p("/m/Downloads");
        let pick = |folders: &[&PathBuf]| overlap(&candidate, folders.iter().map(|f| f.as_path()));

        assert_eq!(pick(&[&apart]), None);
        assert_eq!(
            pick(&[&under]),
            Some((FolderRelation::Contains, under.clone()))
        );
        assert_eq!(
            pick(&[&under, &above]),
            Some((FolderRelation::Inside, above.clone()))
        );
        assert_eq!(
            pick(&[&under, &above, &same, &apart]),
            Some((FolderRelation::Same, same.clone()))
        );
    }

    #[test]
    fn the_managed_folder_is_indexed_by_its_row_or_the_folder_holding_it() {
        let managed = p("/m/Music/WaveFlow");
        let music = (1, p("/m/Music"));
        let deeper = (5, p("/m/Music/"));
        let own = (2, managed.clone());
        let other = (3, p("/m/Downloads"));
        assert_eq!(
            indexing_folder(&managed, std::slice::from_ref(&other)),
            Ok(None)
        );
        assert_eq!(
            indexing_folder(&managed, &[music.clone(), other]),
            Ok(Some(1))
        );
        assert_eq!(
            indexing_folder(&managed, &[music.clone(), own]),
            Ok(Some(2))
        );
        // A trailing separator is not a deeper folder.
        assert_eq!(indexing_folder(&managed, &[music, deeper]), Ok(Some(1)));
        let under = (4, managed.join("Album"));
        assert_eq!(
            indexing_folder(&managed, std::slice::from_ref(&under)),
            Err((FolderRelation::Contains, under.1))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_ignores_case_and_separator_style() {
        assert_eq!(
            relation(&p(r"c:\users\u\music\Albums"), &p("C:/Users/U/Music")),
            Some(FolderRelation::Inside)
        );
    }
}

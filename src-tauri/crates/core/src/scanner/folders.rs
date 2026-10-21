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

use std::path::{Component, Path};

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
    use std::path::PathBuf;

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

    #[cfg(windows)]
    #[test]
    fn windows_ignores_case_and_separator_style() {
        assert_eq!(
            relation(&p(r"c:\users\u\music\Albums"), &p("C:/Users/U/Music")),
            Some(FolderRelation::Inside)
        );
    }
}

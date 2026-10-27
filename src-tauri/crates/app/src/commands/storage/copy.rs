//! Copying a cache tree to its new root.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::AppError;

/// Why [`copy_tree`] stopped before the end.
#[derive(Debug)]
pub(super) enum CopyStop {
    /// The stop flag was raised; the caller rolls back.
    Cancelled,
    Failed(AppError),
}

impl<E: Into<AppError>> From<E> for CopyStop {
    fn from(err: E) -> Self {
        Self::Failed(err.into())
    }
}

/// Copy a directory tree, checking `stop` between two files and calling
/// `on_file` after each one. Returns the number of files written.
pub(super) fn copy_tree(
    from: &Path,
    to: &Path,
    stop: &AtomicBool,
    on_file: &dyn Fn(),
) -> Result<u64, CopyStop> {
    if !from.exists() {
        return Ok(0);
    }
    std::fs::create_dir_all(to)?;
    let mut copied = 0u64;
    let mut stack = vec![(from.to_path_buf(), to.to_path_buf())];
    while let Some((src, dst)) = stack.pop() {
        for entry in std::fs::read_dir(&src)? {
            if stop.load(Ordering::Acquire) {
                return Err(CopyStop::Cancelled);
            }
            let entry = entry?;
            let target = dst.join(entry.file_name());
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                std::fs::create_dir_all(&target)?;
                stack.push((entry.path(), target));
            } else if file_type.is_file() {
                std::fs::copy(entry.path(), &target)?;
                copied += 1;
                on_file();
            }
            // Symlinks are skipped rather than followed or recreated: a
            // cache holds files we downloaded, so a link in there is
            // either someone's manual tinkering or a loop.
        }
    }
    Ok(copied)
}

/// Is `inner` the same as, or underneath, `outer`?
///
/// Used to refuse a move into the tree being moved, which would copy
/// files into their own destination for as long as the disk lasted. The
/// comparison is on the paths as given, canonicalised where possible —
/// `canonicalize` fails on a directory that does not exist yet, which is
/// the normal case for a freshly picked target, so a failure falls back
/// to the literal paths rather than refusing the move.
pub(super) fn is_within(inner: &Path, outer: &Path) -> bool {
    let a = inner.canonicalize().unwrap_or_else(|_| inner.to_path_buf());
    let b = outer.canonicalize().unwrap_or_else(|_| outer.to_path_buf());
    a.starts_with(&b)
}

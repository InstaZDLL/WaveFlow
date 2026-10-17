//! Moving files to the system's trash, where the user can still restore
//! them (#878).
//!
//! One function, three routes:
//!
//! - **Windows** — the Recycle Bin, through the `trash` crate
//!   (`IFileOperation`).
//! - **macOS** — `NSFileManager`'s `trashItemAtURL`. The crate's default
//!   asks Finder through AppleScript, which makes macOS prompt the user
//!   to let WaveFlow control Finder; the file manager call needs no
//!   permission.
//! - **Linux** — the freedesktop trash through the crate, except inside
//!   Flatpak: there the sandbox sees `~/Music` and not the user's trash,
//!   and `$XDG_DATA_HOME` points into the app's own data, so the crate
//!   would "trash" a file into a folder no file manager shows. The
//!   `org.freedesktop.portal.Trash` portal moves it on the host's behalf.
//!
//! The work runs on a thread of its own: the crate initialises COM for
//! the calling thread in apartment mode and panics if that thread already
//! chose another mode, which a pooled thread that once served WASAPI may
//! have.

use std::path::{Path, PathBuf};

/// What happened to one path.
pub type Outcome = Result<(), String>;

/// Move every path to the trash, one at a time, and say what became of
/// each, in the same order. A file that is no longer in its folder counts
/// as done: there is nothing left to move, and its row should go with it.
/// A file whose *folder* is missing does not: that is a drive unplugged
/// or a share down, where every file looks gone and none of them is.
pub async fn move_all(paths: Vec<PathBuf>) -> Vec<Outcome> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("trash".into())
        .spawn(move || {
            let outcomes = paths.iter().map(|p| move_one(p)).collect::<Vec<_>>();
            let _ = tx.send(outcomes);
        });
    if let Err(e) = spawned {
        return vec![Err(format!("could not start the trash thread: {e}"))];
    }
    rx.await
        .unwrap_or_else(|_| vec![Err("the trash thread stopped".into())])
}

fn move_one(path: &Path) -> Outcome {
    if std::fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return if path.parent().is_some_and(Path::is_dir) {
            Ok(())
        } else {
            Err("its folder is unavailable (an unplugged drive or a share that is down?)".into())
        };
    }
    #[cfg(target_os = "linux")]
    if in_flatpak() {
        return portal::trash_file(path);
    }
    context().delete(path).map_err(|e| e.to_string())
}

fn context() -> trash::TrashContext {
    #[allow(unused_mut)]
    let mut ctx = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        ctx.set_delete_method(DeleteMethod::NsFileManager);
    }
    ctx
}

#[cfg(target_os = "linux")]
fn in_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

#[cfg(target_os = "linux")]
mod portal {
    use std::path::Path;

    use zbus::blocking::Connection;
    use zbus::zvariant::Fd;

    /// `org.freedesktop.portal.Trash.TrashFile`: takes a descriptor of
    /// the file, answers 1 when it was moved and 0 when it was not.
    ///
    /// The descriptor is opened **read-write**, which changes nothing in
    /// the file: for a sandboxed caller the portal only trashes what the
    /// app could have written, and judges that from the descriptor's
    /// access mode — a read-only one is refused, always, with a bare 0
    /// (`xdp_app_info_get_path_for_fd` in xdg-desktop-portal).
    pub(super) fn trash_file(path: &Path) -> Result<(), String> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("open: {e}"))?;
        let conn = Connection::session().map_err(|e| format!("session bus: {e}"))?;
        let reply = conn
            .call_method(
                Some("org.freedesktop.portal.Desktop"),
                "/org/freedesktop/portal/desktop",
                Some("org.freedesktop.portal.Trash"),
                "TrashFile",
                &(Fd::from(&file),),
            )
            .map_err(|e| format!("trash portal: {e}"))?;
        let moved: u32 = reply
            .body()
            .deserialize()
            .map_err(|e| format!("trash portal reply: {e}"))?;
        if moved == 1 {
            Ok(())
        } else {
            Err("the trash portal refused the file".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real route on the machine running the test: the file leaves
    /// its folder. On CI that is the freedesktop and Windows trash.
    #[tokio::test]
    async fn a_file_leaves_for_the_trash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("copy.mp3");
        std::fs::write(&path, b"not really audio").expect("write");
        let outcomes = move_all(vec![path.clone()]).await;
        assert_eq!(outcomes, vec![Ok(())]);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_missing_file_counts_as_moved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outcomes = move_all(vec![dir.path().join("gone.mp3")]).await;
        assert_eq!(outcomes, vec![Ok(())]);
    }

    /// An unplugged drive makes every file look gone: its rows must stay.
    #[tokio::test]
    async fn a_missing_folder_is_not_a_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outcomes = move_all(vec![dir.path().join("unplugged").join("copy.mp3")]).await;
        assert!(outcomes[0].is_err(), "{outcomes:?}");
    }
}

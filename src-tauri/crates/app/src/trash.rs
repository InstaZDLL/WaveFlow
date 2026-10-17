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

use std::path::Path;

/// What happened to one path.
pub type Outcome = Result<(), String>;

/// Run `work` on a thread of its own and wait for it. Every
/// [`move_one`] call goes through here, for the COM reason in the module
/// documentation; the caller puts its own checks in `work`, so they run
/// right before each move rather than before the wait.
pub async fn on_trash_thread<R: Send + 'static>(
    work: impl FnOnce() -> R + Send + 'static,
) -> Result<R, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("trash".into())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .map_err(|e| format!("could not start the trash thread: {e}"))?;
    rx.await.map_err(|_| "the trash thread stopped".to_string())
}

/// Move one file to the trash. Call it from [`on_trash_thread`].
///
/// A file that is no longer in its folder counts as done: there is
/// nothing left to move, and its row should go with it. A file whose
/// *folder* is missing does not: that is a drive unplugged or a share
/// down, where every file looks gone and none of them is.
pub fn move_one(path: &Path) -> Outcome {
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
        let moved = path.clone();
        let outcome = on_trash_thread(move || move_one(&moved))
            .await
            .expect("thread");
        assert_eq!(outcome, Ok(()));
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_missing_file_counts_as_moved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gone = dir.path().join("gone.mp3");
        let outcome = on_trash_thread(move || move_one(&gone))
            .await
            .expect("thread");
        assert_eq!(outcome, Ok(()));
    }

    /// An unplugged drive makes every file look gone: its rows must stay.
    #[tokio::test]
    async fn a_missing_folder_is_not_a_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let unplugged = dir.path().join("unplugged").join("copy.mp3");
        let outcome = on_trash_thread(move || move_one(&unplugged))
            .await
            .expect("thread");
        assert!(outcome.is_err(), "{outcome:?}");
    }
}

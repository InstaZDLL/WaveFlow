//! Importing a server track into a scanned folder — lot 5, the half that
//! brings bytes *in*.
//!
//! ## An import is not a download
//!
//! A [download](super::download) lands in a managed folder the scanner never
//! sees; it stays keyed by the server's id, no `track` row is created, and it
//! describes a *remote* track that happens to be on this disk. An import lands
//! in a folder the user already scans: the scanner indexes it, a `track` row
//! exists, and the file is theirs — playable offline, editable, counted in
//! every local view — with the server's track *linked* to it rather than
//! shadowed by it.
//!
//! The two are deliberately separate features rather than one with a flag.
//! They answer different questions: "keep this for the plane" versus "this
//! belongs in my library".
//!
//! ## The proof is free here, and only here
//!
//! Reconciling a local file against the server normally costs a full re-read:
//! the library's `file_hash` is a partial digest and the server's `full_hash`
//! covers the whole file, so the two are incompatible by construction. An
//! import writes the bytes itself, so the server's own digest falls out of the
//! write — one pass, no re-read — and the exact link is written from it
//! directly. That is the reason lot 4 came before this one.
//!
//! ## Refusals are cheap before the first byte
//!
//! Everything that can refuse an import is checked before a single byte is
//! fetched: an extension the local scanner cannot index, a track already
//! linked, or bytes this library already holds. The server settled the same
//! question in the same way for the opposite direction (RFC-008, decision 2),
//! and for the same reason — a refusal after the last byte has already spent
//! the transfer it was supposed to save.
//!
//! ## Already held is a link, not a copy
//!
//! A byte-size prefilter followed by a full hash of the few candidates is what
//! [reconciliation](super::reconciliation) already does across the whole
//! library; here it runs against a single known digest, so it is bounded by
//! the number of local files that happen to share one file's exact size.
//! Finding one means the user already owns these bytes: importing would write
//! a second copy of a file they have. The link is written instead, which is
//! what they would have got from a reconciliation pass anyway.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use serde::Serialize;
use sqlx::{Row, SqlitePool};
use tauri::AppHandle;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// The progress event an import reports on. Distinct from a download's: the
/// two transfers are the same bytes for different features, and one bar must
/// not move for the other's work.
const PROGRESS_EVENT: &str = "remote:import-progress";

/// A track that became a local file.
#[derive(Clone, Serialize)]
pub struct ImportedTrack {
    pub remote_track_id: String,
    pub local_track_id: i64,
    pub path: String,
    pub full_hash: String,
}

/// Why one track was not copied.
///
/// A named verdict rather than a boolean, so the interface can tell what
/// needs no action (`already_linked`, `already_held` — both mean the user has
/// the track) from what will never work (`unsupported_format`) and from what
/// might work later (`failed`).
#[derive(Clone, Copy, Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ImportRefusal {
    /// This server track is already linked to a local one.
    AlreadyLinked,
    /// The library already holds these exact bytes; the link was written
    /// instead of a second copy.
    AlreadyHeld,
    /// The catalogue mirror does not hold this track, so nothing here knows
    /// what to name the file or what its bytes should hash to.
    UnknownTrack,
    /// An extension the local scanner does not index. Copying it would leave a
    /// file in the user's own folder that never becomes a track — clutter with
    /// no way back.
    UnsupportedFormat,
    /// The bytes that arrived are not the bytes the catalogue described.
    HashMismatch,
    /// Written, but the scan did not index it — an unreadable container, or a
    /// codec this build cannot decode. The file is removed rather than left
    /// behind.
    NotIndexed,
    /// The transfer itself failed. Retryable.
    Failed,
}

/// One track that was not copied, and why.
#[derive(Clone, Serialize)]
pub struct SkippedImport {
    pub remote_track_id: String,
    pub reason: ImportRefusal,
    /// The local track involved, for the two verdicts that name one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_track_id: Option<i64>,
}

/// What one import pass did.
#[derive(Clone, Serialize, Default)]
pub struct ImportOutcome {
    pub imported: Vec<ImportedTrack>,
    pub skipped: Vec<SkippedImport>,
}

/// A scanned folder an import may target.
#[derive(Clone, Serialize)]
pub struct ImportFolder {
    pub folder_id: i64,
    pub library_id: i64,
    pub path: String,
    /// False when the path is not on this machine right now — an unplugged
    /// drive, a network share that is down. Offered but not selectable.
    pub exists: bool,
}

/// What the mirror knows about a track, which is everything an import needs.
struct RemoteTrackMeta {
    title: String,
    artist: Option<String>,
    album: Option<String>,
    track_no: Option<i64>,
    disc_no: Option<i64>,
    suffix: Option<String>,
    size: Option<i64>,
    full_hash: Option<String>,
}

/// Track ids being imported right now, so a second click cannot start a second
/// transfer of the same track into the same folder.
fn in_flight() -> &'static Mutex<HashSet<String>> {
    static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Releases its track id however the import ends, including a panic.
struct InFlightGuard(String);

impl InFlightGuard {
    fn claim(track_id: &str) -> Option<Self> {
        let mut set = in_flight().lock().ok()?;
        if !set.insert(track_id.to_string()) {
            return None;
        }
        Some(Self(track_id.to_string()))
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if let Ok(mut set) = in_flight().lock() {
            set.remove(&self.0);
        }
    }
}

/// The scanned folders an import can target, newest library first.
pub async fn folders(pool: &SqlitePool) -> AppResult<Vec<ImportFolder>> {
    let rows =
        sqlx::query("SELECT id, library_id, path FROM library_folder ORDER BY library_id, path")
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let path: String = row.try_get("path").ok()?;
            let exists = Path::new(&path).is_dir();
            Some(ImportFolder {
                folder_id: row.try_get("id").ok()?,
                library_id: row.try_get("library_id").ok()?,
                path,
                exists,
            })
        })
        .collect())
}

/// Copy server tracks into a scanned folder, index them, and link each one to
/// the track it came from.
///
/// Every file is written first and the folder scanned **once** at the end.
/// Scanning per file would re-walk the whole folder for each track, and the
/// scan's own fast path makes one pass over an already-indexed folder cheap
/// while making a hundred passes exactly a hundred times that.
pub async fn import(
    app: &AppHandle,
    state: &AppState,
    remote_track_ids: &[String],
    folder_id: i64,
) -> AppResult<ImportOutcome> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline".into()));
    }
    // Pool and profile together, held across every await below: resolving them
    // separately is what would let a profile switch mid-import file one
    // profile's audio into another's folder.
    let (pool, profile_id) = state.require_profile_snapshot().await?;

    let folder_path: Option<String> =
        sqlx::query_scalar("SELECT path FROM library_folder WHERE id = ?")
            .bind(folder_id)
            .fetch_optional(&*pool)
            .await?;
    let folder_path = folder_path
        .ok_or_else(|| AppError::Other(format!("import: folder {folder_id} not found")))?;
    let folder = PathBuf::from(&folder_path);
    if !folder.is_dir() {
        return Err(AppError::Other(format!(
            "import: {folder_path} is not reachable"
        )));
    }

    let mut outcome = ImportOutcome::default();
    // (remote id, final path, digest) for everything written, resolved into
    // links after the single scan below.
    let mut written: Vec<(String, PathBuf, String)> = Vec::new();

    // Held until the whole pass is over, not merely until each transfer ends.
    // The bytes are only half of an import: the scan that indexes them and the
    // link written from that both happen after the loop, and a second caller
    // slipping in between would find no link, no local track and no file of
    // its own — and would write a second copy of what is already on disk.
    let mut guards: Vec<InFlightGuard> = Vec::new();

    for remote_track_id in remote_track_ids {
        let Some(guard) = InFlightGuard::claim(remote_track_id) else {
            outcome.skipped.push(SkippedImport {
                remote_track_id: remote_track_id.clone(),
                reason: ImportRefusal::Failed,
                local_track_id: None,
            });
            continue;
        };
        guards.push(guard);
        match import_one(app, state, &pool, remote_track_id, &folder).await {
            Ok(Prepared::Written { path, full_hash }) => {
                written.push((remote_track_id.clone(), path, full_hash));
            }
            Ok(Prepared::Refused {
                reason,
                local_track_id,
            }) => outcome.skipped.push(SkippedImport {
                remote_track_id: remote_track_id.clone(),
                reason,
                local_track_id,
            }),
            Err(err) => {
                tracing::warn!(track = %remote_track_id, ?err, "import failed");
                outcome.skipped.push(SkippedImport {
                    remote_track_id: remote_track_id.clone(),
                    reason: ImportRefusal::Failed,
                    local_track_id: None,
                });
            }
        }
    }

    if written.is_empty() {
        return Ok(outcome);
    }
    // `guards` stays alive to the end of this function on purpose; naming it
    // here keeps a later edit from "cleaning up" an unused-looking binding.
    debug_assert!(!guards.is_empty());

    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    crate::commands::scan::scan_folder_inner(&pool, &artwork_dir, folder_id, Some(app), false)
        .await?;

    for (remote_track_id, path, full_hash) in written {
        let path_text = path.to_string_lossy().to_string();
        let local_track_id: Option<i64> =
            sqlx::query_scalar("SELECT id FROM track WHERE file_path = ? AND is_available = 1")
                .bind(&path_text)
                .fetch_optional(&*pool)
                .await?;
        let Some(local_track_id) = local_track_id else {
            // The scanner declined it. Leaving the file would put something in
            // the user's own folder that no view will ever show and nothing
            // will ever offer to remove.
            let _ = std::fs::remove_file(&path);
            outcome.skipped.push(SkippedImport {
                remote_track_id,
                reason: ImportRefusal::NotIndexed,
                local_track_id: None,
            });
            continue;
        };
        match super::reconciliation::link_exact(&pool, local_track_id, &remote_track_id, &full_hash)
            .await
        {
            Ok(()) => outcome.imported.push(ImportedTrack {
                remote_track_id,
                local_track_id,
                path: path_text,
                full_hash,
            }),
            Err(err) => {
                // The file is indexed and playable; only the link failed. That
                // is worth reporting rather than undoing — deleting a track
                // the user can now see would be the larger surprise.
                tracing::warn!(track = %remote_track_id, ?err, "import: link failed");
                outcome.imported.push(ImportedTrack {
                    remote_track_id,
                    local_track_id,
                    path: path_text,
                    full_hash,
                });
            }
        }
    }
    Ok(outcome)
}

/// What preparing one track produced.
enum Prepared {
    Written {
        path: PathBuf,
        full_hash: String,
    },
    Refused {
        reason: ImportRefusal,
        local_track_id: Option<i64>,
    },
}

async fn import_one(
    app: &AppHandle,
    state: &AppState,
    pool: &SqlitePool,
    remote_track_id: &str,
    folder: &Path,
) -> AppResult<Prepared> {
    let linked: Option<i64> = sqlx::query_scalar(
        "SELECT local_track_id FROM remote_track_link WHERE remote_track_id = ?",
    )
    .bind(remote_track_id)
    .fetch_optional(pool)
    .await?;
    if let Some(local_track_id) = linked {
        return Ok(Prepared::Refused {
            reason: ImportRefusal::AlreadyLinked,
            local_track_id: Some(local_track_id),
        });
    }

    let Some(meta) = meta(pool, remote_track_id).await? else {
        return Ok(Prepared::Refused {
            reason: ImportRefusal::UnknownTrack,
            local_track_id: None,
        });
    };
    let extension = extension_for(meta.suffix.as_deref());
    // Checked against the local scanner's own list rather than a copy of it:
    // what this build can index is the only thing that decides whether a file
    // in a scanned folder will ever become a track.
    if !waveflow_core::scanner::AUDIO_EXTENSIONS.contains(&extension.as_str()) {
        return Ok(Prepared::Refused {
            reason: ImportRefusal::UnsupportedFormat,
            local_track_id: None,
        });
    }

    // Bytes we already hold are a link, not a transfer.
    if let (Some(size), Some(expected)) = (meta.size, meta.full_hash.as_deref()) {
        if let Some(local_track_id) = local_twin(pool, size, expected).await? {
            if let Err(err) =
                super::reconciliation::link_exact(pool, local_track_id, remote_track_id, expected)
                    .await
            {
                // The verdict stands either way: the user holds these bytes,
                // and saying so is the useful half. Only the link failed.
                tracing::warn!(track = %remote_track_id, ?err, "import: linking a twin failed");
            }
            return Ok(Prepared::Refused {
                reason: ImportRefusal::AlreadyHeld,
                local_track_id: Some(local_track_id),
            });
        }
    }

    // The two fallbacks stay English rather than localised: they name
    // directories on disk, and translating them would scatter one artist's
    // untagged tracks across as many folders as the user has changed language.
    let dir = folder.join(sanitize_component(
        meta.artist.as_deref().unwrap_or_default(),
        "Unknown Artist",
    ));
    let dir = dir.join(sanitize_component(
        meta.album.as_deref().unwrap_or_default(),
        "Unknown Album",
    ));
    std::fs::create_dir_all(&dir)?;
    let final_path = reserve_path(&dir, &stem(&meta), &extension)?;

    // A working name that carries no audio extension, in the destination
    // directory itself. Both halves matter: same directory means the rename is
    // a rename and not a cross-device copy with a window where a truncated
    // file exists, and no audio extension means a scan crossing it mid-write
    // cannot index a half-written file — the scanner filters on extension, so
    // this is structural rather than a rule someone must remember. Unique per
    // call so two writers can never interleave one file.
    let part_path = dir.join(format!(
        "{}.{}.waveflow-part",
        blake3::hash(remote_track_id.as_bytes()).to_hex(),
        blake3::hash(format!("{:?}", std::time::SystemTime::now()).as_bytes())
            .to_hex()
            .split_at(16)
            .0
    ));

    let url = super::stream::raw_ticket_url(state, remote_track_id).await?;
    let outcome =
        super::download::stream_to_file(app, remote_track_id, &url, &part_path, PROGRESS_EVENT)
            .await;
    let (_size, full_hash) = match outcome {
        Ok(pair) => pair,
        Err(err) => {
            // Both the working file and the empty name we claimed: leaving the
            // reservation behind would put a zero-byte file in the user's own
            // folder that nothing explains.
            let _ = std::fs::remove_file(&part_path);
            let _ = std::fs::remove_file(&final_path);
            return Err(err);
        }
    };
    if let Some(expected) = meta.full_hash.as_deref() {
        if !expected.eq_ignore_ascii_case(&full_hash) {
            let _ = std::fs::remove_file(&part_path);
            let _ = std::fs::remove_file(&final_path);
            return Ok(Prepared::Refused {
                reason: ImportRefusal::HashMismatch,
                local_track_id: None,
            });
        }
    }
    // Lands on the reservation, which is why this is a rename and not a
    // create: the name has been ours since before the first byte.
    if let Err(err) = std::fs::rename(&part_path, &final_path) {
        let _ = std::fs::remove_file(&part_path);
        let _ = std::fs::remove_file(&final_path);
        return Err(AppError::from(err));
    }
    Ok(Prepared::Written {
        path: final_path,
        full_hash,
    })
}

/// What the mirror holds for one track.
async fn meta(pool: &SqlitePool, remote_track_id: &str) -> AppResult<Option<RemoteTrackMeta>> {
    let row = sqlx::query(
        "SELECT title, artist, album, track_no, disc_no, suffix, size, full_hash
           FROM remote_track WHERE remote_id = ?",
    )
    .bind(remote_track_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else { return Ok(None) };
    Ok(Some(RemoteTrackMeta {
        title: row.try_get("title")?,
        artist: row.try_get("artist")?,
        album: row.try_get("album")?,
        track_no: row.try_get("track_no")?,
        disc_no: row.try_get("disc_no")?,
        suffix: row.try_get("suffix")?,
        size: row.try_get("size")?,
        full_hash: row.try_get("full_hash")?,
    }))
}

/// A local track holding exactly these bytes, if the library has one.
///
/// The byte size narrows the field to a handful; the full digest decides.
/// Tracks already linked elsewhere are excluded — linking them would steal a
/// pairing the user or a reconciliation pass already established.
async fn local_twin(pool: &SqlitePool, size: i64, expected_hash: &str) -> AppResult<Option<i64>> {
    if size <= 0 || expected_hash.len() != 64 {
        return Ok(None);
    }
    let candidates: Vec<(i64, String)> = sqlx::query_as(
        "SELECT t.id, t.file_path
           FROM track t
          WHERE t.file_size = ? AND t.is_available = 1
            AND NOT EXISTS (SELECT 1 FROM remote_track_link l WHERE l.local_track_id = t.id)",
    )
    .bind(size)
    .fetch_all(pool)
    .await?;
    for (track_id, file_path) in candidates {
        let path = PathBuf::from(file_path);
        let hashed =
            tokio::task::spawn_blocking(move || waveflow_core::scanner::hash_file_full(&path))
                .await
                .map_err(|err| AppError::Other(format!("import hash task failed: {err}")))?;
        let Ok(hashed) = hashed else { continue };
        if hashed.eq_ignore_ascii_case(expected_hash) {
            return Ok(Some(track_id));
        }
    }
    Ok(None)
}

/// The extension to write, lower-cased and stripped of anything a path cannot
/// carry. Empty when the server declared no container, which the caller then
/// refuses as `unsupported_format` — guessing one would name a file after a
/// format nobody claimed it was in.
fn extension_for(suffix: Option<&str>) -> String {
    suffix
        .unwrap_or_default()
        .trim()
        .trim_start_matches('.')
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase()
}

/// The file name's stem: `NN - Title`, or `D-NN - Title` on a multi-disc
/// release, so a directory listing sorts the way the album plays.
fn stem(meta: &RemoteTrackMeta) -> String {
    let title = sanitize_component(&meta.title, "Untitled");
    match (meta.disc_no, meta.track_no) {
        (Some(disc), Some(track)) if disc > 1 => format!("{disc}-{track:02} - {title}"),
        (_, Some(track)) if track > 0 => format!("{track:02} - {title}"),
        _ => title,
    }
}

/// One path component, safe on every platform the app ships to.
///
/// Deliberately gentler than the app's other filename sanitizers: those name
/// archives, where legibility is a nicety. This one names music in someone's
/// own library, so accents, apostrophes and brackets — most of what titles are
/// actually made of — survive, and only what a filesystem genuinely refuses is
/// replaced.
fn sanitize_component(raw: &str, fallback: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    // Windows refuses a trailing dot or space, and silently strips them
    // elsewhere — which would make two different titles land on one name.
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']).trim();
    // Bounded in characters rather than bytes so a multi-byte title is not cut
    // mid-character, and short enough that artist + album + stem + extension
    // stay inside a 255-byte component limit even in UTF-8's worst case.
    let limited: String = trimmed.chars().take(60).collect();
    let limited = limited.trim_end_matches(['.', ' ']).trim();
    if limited.is_empty() || is_reserved(limited) {
        return fallback.to_string();
    }
    limited.to_string()
}

/// Names Windows refuses whatever the extension.
fn is_reserved(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let stem = name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    RESERVED.contains(&stem.as_str())
}

/// A path in `dir` that nothing occupies, **claimed** rather than merely
/// checked, suffixing instead of overwriting.
///
/// The name is reserved by creating the empty file exclusively, and the final
/// rename lands on that reservation. Testing `exists()` and renaming later
/// leaves a window in which a concurrent import — a second window, a second
/// track that happens to share artist, album and title — picks the same free
/// name and one of the two silently overwrites the other. Never overwrites a
/// file that was already there: the folder is the user's own, and what is in
/// it is theirs whether this feature put it there or not.
fn reserve_path(dir: &Path, stem: &str, extension: &str) -> AppResult<PathBuf> {
    let named = |n: u32| -> PathBuf {
        let base = if n == 1 {
            stem.to_string()
        } else {
            format!("{stem} ({n})")
        };
        if extension.is_empty() {
            dir.join(base)
        } else {
            dir.join(format!("{base}.{extension}"))
        }
    };
    for n in 1..=1000 {
        let candidate = named(n);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(AppError::from(err)),
        }
    }
    // A folder holding a thousand files of one name is not a case worth
    // looping over forever.
    Err(AppError::Other(format!(
        "import: no free name for {stem} in {}",
        dir.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_keeps_what_titles_are_made_of() {
        assert_eq!(
            sanitize_component("Don't Stop (Live)", "x"),
            "Don't Stop (Live)"
        );
        assert_eq!(sanitize_component("Björk — Jóga", "x"), "Björk — Jóga");
    }

    #[test]
    fn a_component_never_escapes_its_directory() {
        assert_eq!(sanitize_component("../../etc", "x"), ".._.._etc");
        assert_eq!(sanitize_component("a/b\\c", "x"), "a_b_c");
        assert_eq!(sanitize_component("..", "fallback"), "fallback");
        assert_eq!(sanitize_component("", "fallback"), "fallback");
        assert_eq!(sanitize_component("   ", "fallback"), "fallback");
    }

    #[test]
    fn windows_refuses_these_and_so_do_we() {
        assert_eq!(sanitize_component("trailing.", "x"), "trailing");
        assert_eq!(sanitize_component("trailing ", "x"), "trailing");
        assert_eq!(sanitize_component("CON", "fallback"), "fallback");
        assert_eq!(sanitize_component("nul.txt", "fallback"), "fallback");
        assert_eq!(sanitize_component("a:b?c*d", "x"), "a_b_c_d");
    }

    #[test]
    fn a_long_title_is_cut_on_a_character_not_a_byte() {
        let long = "é".repeat(200);
        let cut = sanitize_component(&long, "x");
        assert_eq!(cut.chars().count(), 60);
    }

    #[test]
    fn a_hostile_suffix_cannot_reach_the_path() {
        assert_eq!(extension_for(Some("../../sh")), "sh");
        assert_eq!(extension_for(Some(".FLAC")), "flac");
        assert_eq!(extension_for(None), "");
    }

    #[test]
    fn a_stem_sorts_the_way_the_album_plays() {
        let mut meta = RemoteTrackMeta {
            title: "Song".into(),
            artist: None,
            album: None,
            track_no: Some(3),
            disc_no: Some(1),
            suffix: None,
            size: None,
            full_hash: None,
        };
        assert_eq!(stem(&meta), "03 - Song");
        meta.disc_no = Some(2);
        assert_eq!(stem(&meta), "2-03 - Song");
        meta.track_no = None;
        assert_eq!(stem(&meta), "Song");
    }

    #[test]
    fn a_name_is_claimed_not_merely_checked() {
        let dir = std::env::temp_dir().join(format!(
            "waveflow-import-test-{}",
            blake3::hash(format!("{:?}", std::time::SystemTime::now()).as_bytes()).to_hex()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Claiming creates the file, so a second claim must move on without
        // the caller having written anything in between — that gap is the
        // race an `exists()` check leaves open.
        let first = reserve_path(&dir, "01 - Song", "flac").unwrap();
        assert!(first.ends_with("01 - Song.flac"));
        assert!(first.is_file());
        let second = reserve_path(&dir, "01 - Song", "flac").unwrap();
        assert!(second.ends_with("01 - Song (2).flac"), "{second:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_working_file_carries_no_audio_extension() {
        // The invariant the scanner enforces for us: what is incomplete must
        // not be indexable. `is_scannable_audio` filters on extension, so the
        // working name has to fail that filter.
        let name = format!("{}.deadbeef.waveflow-part", blake3::hash(b"t1").to_hex());
        assert!(!waveflow_core::scanner::is_scannable_audio(Path::new(
            &name
        )));
    }
}

//! Offering the bound server the tracks it does not have — lot 5, the half
//! that sends bytes *out*.
//!
//! The mirror image of [`super::import`], and the reason lot 5 is called
//! "balance": a library that can pull from the server and never push to it
//! keeps two collections that drift apart on purpose.
//!
//! ## The digest goes before the bytes
//!
//! Nothing is transferred until the server has been told what it would be
//! receiving and has said it wants it. That is the server's protocol (its
//! RFC-008, decision 2) and it is the only moment a refusal is cheap: a
//! `present`, a closed library or an unsupported container costs one question
//! here and a whole transfer anywhere later.
//!
//! ## Most of the deduplication never reaches the server
//!
//! The catalogue mirror already holds every server track's `full_hash`. A
//! local file whose digest is in that table is a file the server has, and the
//! answer is a *link*, written offline, with no request at all. The server's
//! own RFC says as much — it expects the bulk of this to happen client-side,
//! and sizes its negotiation route for the leftovers rather than for the
//! volume.
//!
//! ## One session at a time, deliberately
//!
//! The negotiation route takes a batch, and this does not use it. A batch does
//! not merely ask questions: every `accepted` verdict **opens a session and
//! reserves that file's size against the library's quota**. Offering two
//! hundred files at once would open two hundred sessions, all but one of them
//! idle, holding quota that nothing is spending — and would run into a
//! per-account session ceiling this client cannot know the value of. Since our
//! deduplication is already done offline, the batch buys nothing here.
//!
//! ## What resumption rests on
//!
//! The session state is *read*, never deduced. A chunk whose acknowledgement
//! was lost is the ordinary shape of a home connection dropping, so the server
//! answers a re-sent chunk idempotently and tells us where it actually is. We
//! ask rather than assume, which is the only way a resumed upload cannot skip
//! a fragment and discover it at the final digest.

use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use tauri::{AppHandle, Emitter};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// Cooperative stop for a sweep that can run for a long time.
///
/// A plain flag rather than the phase machine reconciliation needs: there is
/// no all-or-nothing commit to race against here. Every track that has already
/// been committed is committed for good — the protocol is resumable and the
/// server keeps what it validated — so stopping between two tracks leaves a
/// state that is correct rather than half-written, and the next sweep picks up
/// what is left.
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Whether a survey or an upload owns the slot right now.
///
/// One at a time, and not for the database's sake — both are idempotent. It is
/// [`CANCEL`] that cannot be shared: two runs would clear each other's stop
/// request, so a cancel aimed at the long one would be swallowed by the short
/// one finishing.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Requests cancellation of the active survey or upload run.

///

/// The running operation stops after completing its current track.

///

/// # Examples

///

/// ```

/// request_cancel();

/// ```
pub fn request_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// Claims the slot, and clears both flags on every exit path — early return,
/// `?` or panic — so one cancel cannot poison the next run.
struct RunGuard;

impl RunGuard {
    /// Claims the exclusive run slot and clears any pending cancellation request.
    ///
    /// Returns `Some` when the slot is claimed successfully, or `None` when another
    /// run is already active.
    ///
    /// # Examples
    ///
    /// ```
    /// let guard = RunGuard::claim();
    /// assert!(guard.is_some());
    /// ```
    fn claim() -> Option<Self>
    fn claim() -> Option<Self> {
        RUNNING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()?;
        CANCEL.store(false, Ordering::SeqCst);
        Some(Self)
    }
}

impl Drop for RunGuard {
    /// Releases the run guard and clears the cancellation request.
    ///
    /// # Examples
    ///
    /// ```
    /// // Dropping the guard releases the exclusive run slot.
    /// drop(guard);
    /// ```
    fn drop(&mut self) {
        CANCEL.store(false, Ordering::SeqCst);
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Reports whether cancellation has been requested for the current operation.
///
/// # Examples
///
/// ```
/// let cancellation_requested = cancelled();
/// assert!(cancellation_requested || !cancellation_requested);
/// ```
fn cancelled() -> bool {
    CANCEL.load(Ordering::SeqCst)
}

/// A server library, as an upload destination.
#[derive(Clone, Serialize)]
pub struct UploadLibrary {
    pub library_id: String,
    pub name: String,
}

/// One local track the server does not appear to have.
#[derive(Clone, Serialize)]
pub struct UploadCandidate {
    pub track_id: i64,
    pub title: String,
    pub artist: Option<String>,
    pub size: i64,
    pub full_hash: String,
    pub extension: String,
}

/// What a survey of the local library found.
#[derive(Clone, Serialize, Default)]
pub struct UploadPlan {
    pub candidates: Vec<UploadCandidate>,
    /// Linked without a single request: their digest was already in the
    /// mirrored catalogue.
    pub linked_offline: usize,
    /// Files that could not be read at all.
    pub unreadable: usize,
    /// Containers the server would refuse, so they are never offered.
    pub unsupported: usize,
    /// True when the survey stopped early.
    pub cancelled: bool,
}

/// Progress of the survey, which is the expensive half: it reads every
/// unlinked file once.
#[derive(Clone, Serialize)]
struct SurveyProgress {
    processed: usize,
    total: usize,
}

/// Progress of one transfer.
#[derive(Clone, Serialize)]
struct UploadProgress {
    track_id: i64,
    sent: i64,
    total: i64,
}

/// Why one track was not sent. The server's verdicts, plus the two this side
/// decides on its own.
#[derive(Clone, Copy, Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum UploadRefusal {
    /// The library already holds these bytes; the link was written instead.
    Present,
    /// The container is not one the server indexes.
    UnsupportedFormat,
    /// Above the server's per-file ceiling.
    TooLarge,
    /// The library has no room. Worth retrying later.
    QuotaExceeded,
    /// The library does not accept uploads. Its operator decides that, not us.
    LibraryClosed,
    /// The account holds as many sessions as it may. Retryable.
    TooManySessions,
    /// A verdict this build does not know. Never retried blindly.
    UnknownVerdict,
    /// The file could not be read, or the transfer failed.
    Failed,
}

#[derive(Clone, Serialize)]
pub struct UploadedTrack {
    pub track_id: i64,
    pub remote_track_id: String,
    pub full_hash: String,
}

#[derive(Clone, Serialize)]
pub struct SkippedUpload {
    pub track_id: i64,
    pub reason: UploadRefusal,
}

#[derive(Clone, Serialize, Default)]
pub struct UploadOutcome {
    pub uploaded: Vec<UploadedTrack>,
    pub skipped: Vec<SkippedUpload>,
    pub cancelled: bool,
}

// ---- wire shapes -------------------------------------------------------

#[derive(Serialize)]
struct WireOffer {
    full_hash: String,
    size_bytes: i64,
    extension: String,
}

#[derive(Serialize)]
struct NegotiateBody {
    offers: Vec<WireOffer>,
}

#[derive(Deserialize)]
struct NegotiateResponse {
    verdicts: Vec<WireVerdict>,
}

#[derive(Deserialize)]
struct WireVerdict {
    #[allow(dead_code)]
    full_hash: String,
    decision: String,
    #[serde(default)]
    track_id: Option<String>,
    #[serde(default)]
    session: Option<WireSession>,
}

#[derive(Deserialize, Clone)]
struct WireSession {
    session_id: String,
    next_chunk: i64,
    #[allow(dead_code)]
    received_bytes: i64,
    chunk_bytes: i64,
    #[allow(dead_code)]
    expires_at: i64,
}

#[derive(Deserialize)]
struct WireCommitted {
    track_id: String,
    full_hash: String,
}

// ---- survey ------------------------------------------------------------

/// Lists the remote libraries known to the profile, ordered by name.
///
/// The list identifies available destinations; upload eligibility is determined by the
/// server when an upload is negotiated.
///
/// # Examples
///
/// ```no_run
/// # async fn example() -> app::AppResult<()> {
/// let pool = sqlx::SqlitePool::connect("sqlite://profile.db").await?;
/// let libraries = app::remote::upload::libraries(&pool).await?;
/// # let _ = libraries;
/// # Ok(())
/// # }
/// ```
pub async fn libraries(pool: &SqlitePool) -> AppResult<Vec<UploadLibrary>> {
    let rows = sqlx::query("SELECT remote_id, name FROM remote_library ORDER BY name")
        .fetch_all(pool)
        .await?;
    rows.into_iter()
        .map(|row| {
            Ok(UploadLibrary {
                library_id: row.try_get("remote_id")?,
                name: row.try_get("name")?,
            })
        })
        .collect()
}

/// Surveys unlinked local tracks and identifies files that require remote upload.
///
/// Supported files are hashed, linked locally when their hashes are already present
/// in the remote catalogue, and otherwise returned as upload candidates. The survey
/// can be cancelled between tracks and emits progress updates while processing.
///
/// # Examples
///
/// ```no_run
/// # async fn example(
/// #     app: &tauri::AppHandle,
/// #     state: &crate::AppState,
/// # ) -> crate::AppResult<()> {
/// let plan = crate::remote::upload::survey(app, state).await?;
/// println!("{} files need uploading", plan.candidates.len());
/// # Ok(())
/// # }
/// ```
pub async fn survey(app: &AppHandle, state: &AppState) -> AppResult<UploadPlan> {
    let _guard = RunGuard::claim()
        .ok_or_else(|| AppError::Other("an upload sweep is already running".into()))?;
    let (pool, _) = state.require_profile_snapshot().await?;

    let tracks = super::hashing::unlinked_tracks(&pool).await?;
    let total = tracks.len();
    let mut plan = UploadPlan::default();

    for (index, (track_id, path, size, modified)) in tracks.into_iter().enumerate() {
        if cancelled() {
            plan.cancelled = true;
            break;
        }
        if index % 25 == 0 {
            let _ = app.emit(
                "remote:upload-survey",
                SurveyProgress {
                    processed: index,
                    total,
                },
            );
        }
        let extension = extension_of(&path);
        // Offering a container the server cannot index would spend a transfer
        // on something its catalogue could never show. Checked against the
        // same list the server checks, which both sides get from this
        // workspace's core crate rather than from a copy of each other's.
        if !waveflow_core::scanner::AUDIO_EXTENSIONS.contains(&extension.as_str()) {
            plan.unsupported += 1;
            continue;
        }
        let Some(full_hash) =
            super::hashing::full_hash(&pool, track_id, &path, size, modified).await?
        else {
            plan.unreadable += 1;
            continue;
        };

        // The mirror is the cheap half of the deduplication: a digest already
        // in the catalogue is a file the server has, and the answer is a link
        // rather than a transfer.
        let known: Option<String> =
            sqlx::query_scalar("SELECT remote_id FROM remote_track WHERE full_hash = ? LIMIT 1")
                .bind(&full_hash)
                .fetch_optional(&*pool)
                .await?;
        if let Some(remote_track_id) = known {
            match super::reconciliation::link_exact(&pool, track_id, &remote_track_id, &full_hash)
                .await
            {
                Ok(()) => {
                    plan.linked_offline += 1;
                    continue;
                }
                Err(err) => {
                    // Already linked elsewhere, most likely. Not a candidate
                    // either way: the server holds these bytes.
                    tracing::debug!(
                        track = track_id,
                        ?err,
                        "upload survey: offline link refused"
                    );
                    plan.linked_offline += 1;
                    continue;
                }
            }
        }

        let row = sqlx::query("SELECT title, artist_display FROM track WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&*pool)
            .await?;
        let (title, artist) = match row {
            Some(row) => (
                row.try_get::<String, _>("title").unwrap_or_default(),
                row.try_get::<Option<String>, _>("artist_display")
                    .unwrap_or_default(),
            ),
            None => (String::new(), None),
        };
        plan.candidates.push(UploadCandidate {
            track_id,
            title,
            artist,
            size,
            full_hash,
            extension,
        });
    }

    let _ = app.emit(
        "remote:upload-survey",
        SurveyProgress {
            processed: total,
            total,
        },
    );
    Ok(plan)
}

// ---- transfer ----------------------------------------------------------

/// Uploads the specified local tracks to a remote library sequentially.
///
/// # Arguments
///
/// * `library_id` — Identifier of the destination remote library.
/// * `track_ids` — Identifiers of the local tracks to offer for upload.
///
/// # Returns
///
/// The tracks uploaded, skipped with refusal reasons, and whether cancellation stopped the operation.
///
/// # Errors
///
/// Returns an error when offline, when another upload run is active, or when the active profile cannot be loaded.
///
/// # Examples
///
/// ```no_run
/// # let app = todo!();
/// # let state = todo!();
/// # let library_id = "library-id";
/// # let track_ids = vec![1, 2, 3];
/// let outcome = upload(&app, &state, library_id, &track_ids).await?;
/// # let _: UploadOutcome = outcome;
/// # Ok::<(), AppError>(())
/// ```
pub async fn upload(
    app: &AppHandle,
    state: &AppState,
    library_id: &str,
    track_ids: &[i64],
) -> AppResult<UploadOutcome> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline".into()));
    }
    let _guard = RunGuard::claim()
        .ok_or_else(|| AppError::Other("an upload sweep is already running".into()))?;
    let (pool, _) = state.require_profile_snapshot().await?;

    let mut outcome = UploadOutcome::default();
    for track_id in track_ids {
        if cancelled() {
            outcome.cancelled = true;
            break;
        }
        match upload_one(app, state, &pool, library_id, *track_id).await {
            Ok(Sent::Uploaded(entry)) => outcome.uploaded.push(entry),
            // Set here rather than only at the top of the loop: a stop during
            // the last track's transfer would otherwise fall out of the loop
            // without the flag ever being read, and the run would report
            // itself as having finished.
            Ok(Sent::Cancelled) => {
                outcome.cancelled = true;
                break;
            }
            Ok(Sent::Refused(reason)) => outcome.skipped.push(SkippedUpload {
                track_id: *track_id,
                reason,
            }),
            Err(err) => {
                tracing::warn!(track = track_id, ?err, "upload failed");
                outcome.skipped.push(SkippedUpload {
                    track_id: *track_id,
                    reason: UploadRefusal::Failed,
                });
            }
        }
    }
    Ok(outcome)
}

enum Sent {
    Uploaded(UploadedTrack),
    Refused(UploadRefusal),
    /// Stopped part-way through this track's transfer. Distinct from a
    /// refusal: nothing was decided about the file, and the session holds
    /// what it received, so the next sweep carries on from there.
    Cancelled,
}

/// Uploads one available track to a remote library, resuming any negotiated session.
///
/// Tracks already present remotely are linked locally. Unsupported or rejected tracks
/// are reported as refusals, while cancellation preserves the resumable upload state.
///
/// # Errors
///
/// Returns an error if the track is unavailable, the remote session is invalid or
/// stalls, a transfer fails, or the committed content hash does not match the local hash.
///
/// # Examples
///
/// ```ignore
/// let result = upload_one(&app, &state, &pool, "library-id", track_id).await?;
/// ```
async fn upload_one(
async fn upload_one(
    app: &AppHandle,
    state: &AppState,
    pool: &SqlitePool,
    library_id: &str,
    track_id: i64,
) -> AppResult<Sent> {
    let row = sqlx::query(
        "SELECT file_path, file_size, file_modified FROM track
          WHERE id = ? AND is_available = 1",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Other(format!("upload: track {track_id} is unavailable")))?;
    let path: String = row.try_get("file_path")?;
    let size: i64 = row.try_get("file_size")?;
    let modified: i64 = row.try_get("file_modified")?;

    let extension = extension_of(&path);
    if !waveflow_core::scanner::AUDIO_EXTENSIONS.contains(&extension.as_str()) {
        return Ok(Sent::Refused(UploadRefusal::UnsupportedFormat));
    }
    let Some(full_hash) = super::hashing::full_hash(pool, track_id, &path, size, modified).await?
    else {
        return Ok(Sent::Refused(UploadRefusal::Failed));
    };

    let client = super::client::RemoteClient::try_build(state)
        .await?
        .ok_or_else(|| AppError::Other("not signed in to a remote server".into()))?;
    let negotiated: NegotiateResponse = client
        .send_json(
            client
                .request(
                    reqwest::Method::POST,
                    &format!("/api/v2/libraries/{library_id}/uploads"),
                )
                .json(&NegotiateBody {
                    offers: vec![WireOffer {
                        full_hash: full_hash.clone(),
                        size_bytes: size,
                        extension: extension.clone(),
                    }],
                }),
        )
        .await
        .map_err(|err| AppError::Other(format!("upload negotiation: {}", err.message)))?;

    let verdict = negotiated
        .verdicts
        .into_iter()
        .next()
        .ok_or_else(|| AppError::Other("upload negotiation: no verdict".into()))?;

    match verdict.decision.as_str() {
        "present" => {
            // The server already holds these bytes. The identifier it hands
            // back is what saves a catalogue sweep to find our own file.
            if let Some(remote_track_id) = verdict.track_id.as_deref() {
                if let Err(err) =
                    super::reconciliation::link_exact(pool, track_id, remote_track_id, &full_hash)
                        .await
                {
                    tracing::warn!(
                        track = track_id,
                        ?err,
                        "upload: linking a present track failed"
                    );
                }
            }
            return Ok(Sent::Refused(UploadRefusal::Present));
        }
        "unsupported_format" => return Ok(Sent::Refused(UploadRefusal::UnsupportedFormat)),
        "too_large" => return Ok(Sent::Refused(UploadRefusal::TooLarge)),
        "quota_exceeded" => return Ok(Sent::Refused(UploadRefusal::QuotaExceeded)),
        "library_closed" => return Ok(Sent::Refused(UploadRefusal::LibraryClosed)),
        "too_many_sessions" => return Ok(Sent::Refused(UploadRefusal::TooManySessions)),
        "accepted" => {}
        // A verdict added after this build. Treated as a refusal rather than
        // as an acceptance: guessing that an unknown answer meant "go ahead"
        // is how a client spends a transfer against a server that said no.
        _ => return Ok(Sent::Refused(UploadRefusal::UnknownVerdict)),
    }

    let mut session = verdict
        .session
        .ok_or_else(|| AppError::Other("upload: accepted without a session".into()))?;
    // Pinned to what the session was opened with. Every offset is
    // `index * chunk_bytes`, so a server that changed the figure mid-transfer
    // would silently move where fragment N is meant to start — a corruption
    // the final digest would catch only after paying for the whole file.
    let chunk_bytes = session.chunk_bytes;
    if chunk_bytes <= 0 {
        return Err(AppError::Other(
            "upload: server asked for empty chunks".into(),
        ));
    }

    while session.next_chunk * chunk_bytes < size {
        if cancelled() {
            // Nothing is lost: the session holds what it received and the next
            // sweep reads its state and carries on from there. Reported as a
            // stop rather than as a failure — the user asked for this, and a
            // track listed under "failed" reads as something to investigate.
            return Ok(Sent::Cancelled);
        }
        let offset = session.next_chunk * chunk_bytes;
        let want = chunk_bytes.min(size - offset);
        let chunk = read_chunk(&path, offset, want as usize).await?;
        let index = session.next_chunk;
        session = client
            .send_json(
                client
                    .request(
                        reqwest::Method::PUT,
                        &format!("/api/v2/uploads/{}/chunks/{index}", session.session_id),
                    )
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .body(chunk),
            )
            .await
            .map_err(|err| AppError::Other(format!("upload chunk {index}: {}", err.message)))?;
        // The loop's only exit is the server moving forward. A state that does
        // not advance — a fragment answered as already-received without the
        // pointer changing — would otherwise re-send the same bytes for ever.
        if session.next_chunk <= index {
            return Err(AppError::Other(format!(
                "upload: session stalled at chunk {index}"
            )));
        }
        if session.chunk_bytes != chunk_bytes {
            return Err(AppError::Other(
                "upload: server changed the fragment size mid-transfer".into(),
            ));
        }
        let _ = app.emit(
            "remote:upload-progress",
            UploadProgress {
                track_id,
                sent: (session.next_chunk * chunk_bytes).min(size),
                total: size,
            },
        );
    }

    let committed: WireCommitted = client
        .send_json(client.request(
            reqwest::Method::POST,
            &format!("/api/v2/uploads/{}/commit", session.session_id),
        ))
        .await
        .map_err(|err| AppError::Other(format!("upload commit: {}", err.message)))?;

    // The server recomputes the digest from the bytes it received and refuses
    // a mismatch, so this can only disagree if one of us is wrong about which
    // file this was. Writing the link anyway would record a proof neither side
    // can stand behind.
    if !committed.full_hash.eq_ignore_ascii_case(&full_hash) {
        return Err(AppError::Other(format!(
            "upload: server hashed track {track_id} differently"
        )));
    }
    if let Err(err) =
        super::reconciliation::link_exact(pool, track_id, &committed.track_id, &full_hash).await
    {
        // The file is on the server and the catalogue has it. Only the local
        // link failed, and a reconciliation pass would establish it later.
        tracing::warn!(
            track = track_id,
            ?err,
            "upload: linking a committed track failed"
        );
    }
    Ok(Sent::Uploaded(UploadedTrack {
        track_id,
        remote_track_id: committed.track_id,
        full_hash,
    }))
}

/// Reads an exact byte fragment from a file at the specified offset.
///
/// # Examples
///
/// ```
/// # #[tokio::main]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let path = std::env::temp_dir().join(format!("read-chunk-{}", std::process::id()));
/// std::fs::write(&path, b"abcdef")?;
///
/// let chunk = read_chunk(path.to_str().unwrap(), 2, 3).await?;
/// assert_eq!(chunk, b"cde");
///
/// std::fs::remove_file(path)?;
/// # Ok(())
/// # }
/// ```
async fn read_chunk(path: &str, offset: i64, len: usize) -> AppResult<Vec<u8>> {
    let owned = path.to_string();
    tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        let mut file = std::fs::File::open(&owned)?;
        file.seek(SeekFrom::Start(offset as u64))?;
        let mut buffer = vec![0u8; len];
        file.read_exact(&mut buffer)?;
        Ok(buffer)
    })
    .await
    .map_err(|err| AppError::Other(format!("upload read task failed: {err}")))?
    .map_err(AppError::from)
}

/// Extracts a path's extension in lowercase without its leading dot.
///
/// # Examples
///
/// ```
/// assert_eq!(extension_of("Music/Track.MP3"), "mp3");
/// assert_eq!(extension_of("README"), "");
/// ```
fn extension_of(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_extension_is_matched_the_way_the_scanner_matches_it() {
        assert_eq!(extension_of("/m/a.FLAC"), "flac");
        assert_eq!(extension_of("/m/a"), "");
        assert!(
            waveflow_core::scanner::AUDIO_EXTENSIONS.contains(&extension_of("/m/a.Mp3").as_str())
        );
    }

    #[test]
    fn a_cancel_does_not_survive_the_run_that_saw_it() {
        let guard = RunGuard::claim().expect("the slot is free");
        request_cancel();
        assert!(cancelled());
        // A second sweep cannot start while this one holds the slot, which is
        // what keeps one run from clearing the other's stop request.
        assert!(RunGuard::claim().is_none());
        drop(guard);
        assert!(!cancelled(), "a cancel must not poison the next sweep");
        assert!(RunGuard::claim().is_some(), "the slot must be free again");
    }
}

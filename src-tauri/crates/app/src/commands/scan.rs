use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, UNIX_EPOCH};

use futures::StreamExt;
use lofty::file::TaggedFileExt;
use lofty::prelude::{Accessor, AudioFile};
use serde::Serialize;
use sqlx::SqlitePool;
use tauri::Emitter;
use walkdir::WalkDir;

use waveflow_core::scanner::{
    extract_album_artist, extract_artist_image, extract_compilation_flag, extract_cover,
    extract_folder_cover, extract_musical_key, extract_rating, file_type_label, hash_file,
    link_local_artist_image, link_va_artist_image, maybe_link_artist_images,
    merge_implicit_compilations, now_millis, split_artist_name, upsert_album, upsert_artist,
    upsert_artwork, upsert_genre, ExtractedFile, AUDIO_EXTENSIONS, VARIOUS_ARTISTS_LABEL,
};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

// Re-export `canonical_name` so existing call sites
// (`crate::commands::scan::canonical_name` in `commands/radio.rs` and
// `commands/similar.rs`) keep resolving after the helper moved to
// `waveflow_core::scanner` in step 6.d.
pub use waveflow_core::scanner::canonical_name;

/// Process-wide count of in-flight library scans. Bumped for the whole
/// body of [`scan_folder_inner`] — which every scan entry point routes
/// through (`scan_folder`, `rescan_library`, `import_paths`, the
/// fs-watcher, the startup rescan in `lib.rs`).
///
/// The background library analyzer reads this via [`scan_in_flight`]
/// and parks itself while a scan is running: a scan is foreground (the
/// user watches its progress toast), saturates every CPU core through
/// the parallel extraction pipeline, and hammers the single SQLite
/// writer. Letting the analyzer's CPU-heavy decode + per-track writes
/// run through it both inflates the scan's wall-clock (a clean ~34 s
/// AAC scan ballooned past a minute under a concurrent analysis pass —
/// measured) and loses analysis rows to lock contention. See
/// [`crate::commands::analysis::run_analyze_library`].
static SCANS_IN_FLIGHT: AtomicU32 = AtomicU32::new(0);

/// `true` while at least one library scan is walking + writing.
pub(crate) fn scan_in_flight() -> bool {
    SCANS_IN_FLIGHT.load(Ordering::Acquire) > 0
}

/// RAII guard that bumps [`SCANS_IN_FLIGHT`] for the lifetime of a scan
/// and decrements it on every exit path (early `?` return, error,
/// panic). One is held at the top of [`scan_folder_inner`].
struct ScanInFlightGuard;

impl ScanInFlightGuard {
    fn new() -> Self {
        SCANS_IN_FLIGHT.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for ScanInFlightGuard {
    fn drop(&mut self) {
        SCANS_IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Payload of the `scan:progress` Tauri event. Emitted by
/// `scan_folder_inner` every ~25 processed files (and once at the end)
/// so the frontend can render a non-blocking toast with the current
/// state of the scan instead of leaving the user staring at a frozen
/// UI for half a minute.
#[derive(Clone, Serialize)]
pub struct ScanProgress {
    pub folder_id: i64,
    pub current: usize,
    pub total: usize,
    pub added: u32,
    pub updated: u32,
    pub skipped: u32,
    pub errors: u32,
    pub done: bool,
}

/// Emit a progress tick — best-effort, errors are swallowed because
/// progress feedback should never abort a scan.
fn maybe_emit_progress(
    app: Option<&tauri::AppHandle>,
    folder_id: i64,
    current: usize,
    total: usize,
    summary: &ScanSummary,
) {
    let Some(app) = app else { return };
    // Ticks every 25 files cap the event volume at ~40 events/s on a
    // hot CPU — enough to feel live without flooding the IPC channel.
    if current != total && current % 25 != 0 {
        return;
    }
    let _ = app.emit(
        "scan:progress",
        ScanProgress {
            folder_id,
            current,
            total,
            added: summary.added,
            updated: summary.updated,
            skipped: summary.skipped,
            errors: summary.errors,
            done: false,
        },
    );
}

/// Outcome of a `scan_folder` call, returned to the frontend so the UI can
/// display a toast like "120 nouveaux titres · 3 mises à jour · 1 erreur".
#[derive(Debug, Serialize, Default)]
pub struct ScanSummary {
    pub folder_id: i64,
    pub scanned: u32,
    pub added: u32,
    pub updated: u32,
    pub skipped: u32,
    pub errors: u32,
    /// Tracks marked `is_available = 0` because their file vanished
    /// from disk between scans. The row stays around (and keeps its
    /// liked / playlist / play-event history) so the user can recover
    /// it by putting the file back.
    pub removed: u32,
}

/// Build the standard `ExtractedFile` payload for a DSF / DFF file.
///
/// Bypasses lofty (which doesn't recognise DSD containers) and goes
/// through the in-tree [`audio::dsd`](crate::audio::dsd) module:
/// [`parser`](crate::audio::dsd::parser) for layout (rate, channels,
/// duration) and [`metadata`](crate::audio::dsd::metadata) for tags.
///
/// `bit_depth` stays at `1` so the Hi-Res badge logic light up the
/// "DSD" pill instead of treating it as 1-bit lossy junk; `codec`
/// reports the human-readable rate label (`DSD64`, `DSD128`, …).
/// No embedded cover extraction — the bare DSF/DFF specs don't
/// reserve a picture frame, so the folder cover fallback (cover.jpg
/// next to the track) does the heavy lifting for DSD libraries.
fn extract_dsd_file(
    path: &Path,
    artwork_dir: &Path,
    size: i64,
    modified_ms: i64,
    hash: String,
    ext: &str,
) -> Result<ExtractedFile, String> {
    use waveflow_core::audio_format::dsd::metadata::read_metadata;
    use waveflow_core::audio_format::dsd::parser::{parse_dff, parse_dsf, DsdContainer};

    let mut file = std::fs::File::open(path).map_err(|e| format!("dsd open: {e}"))?;
    let layout = match ext {
        "dsf" => parse_dsf(&mut file).map_err(|e| format!("dsf parse: {e}"))?,
        "dff" => parse_dff(&mut file).map_err(|e| format!("dff parse: {e}"))?,
        _ => return Err(format!("unexpected DSD ext: {ext}")),
    };
    let meta = read_metadata(&mut file, layout.container).unwrap_or_default();

    let title = meta.title.clone().unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string()
    });
    let codec = layout
        .dsd_rate_multiple()
        .map(|m| format!("DSD{m}"))
        .or_else(|| {
            Some(match layout.container {
                DsdContainer::Dsf => "DSF".to_string(),
                DsdContainer::Dff => "DFF".to_string(),
            })
        });

    Ok(ExtractedFile {
        abs_path: path.to_string_lossy().to_string(),
        size,
        modified_ms,
        hash,
        title,
        artist: meta.artist,
        album: meta.album,
        // The DSF ID3v2 blob / DFF DIIN chunks could carry these but
        // our reader doesn't surface them today; the album grouping
        // falls back to the per-track Artist exactly like before for
        // DSD files. Tagging DSD rips is niche enough that this is
        // OK as a v1 limitation.
        album_artist: None,
        is_compilation: false,
        genre: meta.genre,
        year: meta.year,
        track_number: meta.track_number,
        disc_number: meta.disc_number,
        duration_ms: layout.duration_ms() as i64,
        // No bitrate concept for DSD — leave None rather than
        // computing rate * channels (would mislead the Hi-Res badge).
        bitrate: None,
        sample_rate: Some(layout.sample_rate_hz as i64),
        channels: Some(layout.channels.count() as i64),
        // Mark as 1-bit so the UI knows this is DSD (not lossy junk).
        // The Hi-Res badge logic in src/utils/hires.ts handles the
        // DSD case via the `codec` field starting with "DSD".
        bit_depth: Some(1),
        codec,
        musical_key: None,
        // No embedded picture in DSF/DFF; folder-cover fallback
        // takes over via extract_folder_cover (called below).
        cover_art: extract_folder_cover(path, artwork_dir),
        rating: None,
    })
}

/// Single-file extraction dispatcher: branches DSF/DFF onto the
/// in-tree `audio::dsd` pipeline (symphonia/lofty don't read DSD) and
/// everything else through lofty.
/// Cumulative per-phase timing for one scan, summed across all the
/// parallel extraction tasks (so totals can exceed the wall-clock — the
/// scan summary divides by the parallelism to gauge saturation). Pure
/// diagnostics: lets us see whether a slow scan is hash-bound (BLAKE3
/// full-file read) or tag-bound (lofty) before choosing a fix.
#[derive(Default)]
struct ScanTimings {
    hash_us: AtomicU64,
    tag_us: AtomicU64,
    /// Wall time spent on the SERIAL per-track DB work in the consumer
    /// loop (the `SELECT existing` probe + every `upsert_*` + the row
    /// INSERT/UPDATE + the periodic `TX_BATCH` commit). Unlike
    /// `hash_us` / `tag_us` — summed across the parallel extraction
    /// tasks — this accrues on the single consumer task, so the total
    /// is already wall-clock: it tells us directly how much of
    /// `extract_db_ms` is the single-writer DB path vs the parallel
    /// extraction feeding it. Diagnostics only.
    db_us: AtomicU64,
}

/// Scan-scoped memo for the `artist` / `genre` lookups that otherwise
/// fire one `SELECT … WHERE canonical_name = ?` per track. A 900-track
/// library typically resolves to ~100 distinct artists and ~20 genres,
/// so without this the consumer loop pays thousands of redundant
/// single-writer round-trips — the dominant cost once the partial hash
/// took hashing off the critical path (`db_ms_total` ≈ 99 % of
/// `extract_db_ms`, measured).
///
/// Keyed on [`canonical_name`] exactly like [`upsert_artist`] /
/// [`upsert_genre`] so a cache hit returns the same id the SELECT
/// would. Ids stay valid across the loop's periodic `TX_BATCH` commits
/// (the rows they point at are committed, never rolled back — any error
/// aborts the whole scan and drops the cache with it). `album` is
/// deliberately NOT memoised: [`upsert_album`] carries sticky
/// compilation / album-artist backfill logic that must run per track.
#[derive(Default)]
struct UpsertCache {
    artists: HashMap<String, i64>,
    genres: HashMap<String, i64>,
}

impl UpsertCache {
    /// Cached [`upsert_artist`]. Mirrors its trim → `canonical_name` →
    /// empty-guard so the cache key matches the DB lookup key.
    async fn artist(
        &mut self,
        conn: &mut sqlx::SqliteConnection,
        raw_name: &str,
    ) -> AppResult<Option<i64>> {
        let canon = canonical_name(raw_name.trim());
        if canon.is_empty() {
            return Ok(None);
        }
        if let Some(&id) = self.artists.get(&canon) {
            return Ok(Some(id));
        }
        let id = upsert_artist(conn, raw_name).await?;
        if let Some(id) = id {
            self.artists.insert(canon, id);
        }
        Ok(id)
    }

    /// Cached equivalent of [`upsert_artist_list`] — splits the raw
    /// multi-artist string the same way, then resolves each through
    /// [`Self::artist`].
    async fn artist_list(
        &mut self,
        conn: &mut sqlx::SqliteConnection,
        raw: &Option<String>,
    ) -> AppResult<Vec<i64>> {
        let Some(raw) = raw else {
            return Ok(Vec::new());
        };
        let mut ids = Vec::new();
        for name in split_artist_name(raw) {
            if let Some(id) = self.artist(&mut *conn, &name).await? {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    /// Cached [`upsert_genre`].
    async fn genre(
        &mut self,
        conn: &mut sqlx::SqliteConnection,
        raw_name: &str,
    ) -> AppResult<Option<i64>> {
        let canon = canonical_name(raw_name.trim());
        if canon.is_empty() {
            return Ok(None);
        }
        if let Some(&id) = self.genres.get(&canon) {
            return Ok(Some(id));
        }
        let id = upsert_genre(conn, raw_name).await?;
        if let Some(id) = id {
            self.genres.insert(canon, id);
        }
        Ok(id)
    }
}

fn extract_file(
    path: &Path,
    artwork_dir: &Path,
    timings: &ScanTimings,
) -> Result<ExtractedFile, String> {
    let metadata = fs::metadata(path).map_err(|e| format!("metadata: {e}"))?;
    let size = metadata.len() as i64;
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let t_hash = Instant::now();
    let hash = hash_file(path).map_err(|e| format!("hash: {e}"))?;
    timings
        .hash_us
        .fetch_add(t_hash.elapsed().as_micros() as u64, Ordering::Relaxed);

    // DSD has its own pipeline — symphonia/lofty don't read DSF/DFF.
    // Branch up-front so the rest of the function can keep using
    // lofty unchanged for every other format.
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        let ext_lower = ext.to_ascii_lowercase();
        if matches!(ext_lower.as_str(), "dsf" | "dff") {
            return extract_dsd_file(path, artwork_dir, size, modified_ms, hash, &ext_lower);
        }
    }

    let t_tag = Instant::now();
    let tagged = lofty::read_from_path(path).map_err(|e| format!("lofty: {e}"))?;
    timings
        .tag_us
        .fetch_add(t_tag.elapsed().as_micros() as u64, Ordering::Relaxed);
    let props = tagged.properties();
    let duration_ms = props.duration().as_millis() as i64;
    let bitrate = props.audio_bitrate().map(|b| b as i64);
    let sample_rate = props.sample_rate().map(|s| s as i64);
    let channels = props.channels().map(|c| c as i64);
    // Bit depth: lossless codecs report a real PCM bit count; lossy
    // formats either return None or 0 (which we coalesce away so the
    // UI doesn't badge a 320 kbps MP3 as "0-bit Hi-Res").
    let bit_depth = props.bit_depth().map(|b| b as i64).filter(|d| *d > 0);
    let codec = file_type_label(tagged.file_type());

    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let (
        title,
        artist,
        album,
        album_artist,
        is_compilation,
        genre,
        year,
        track_number,
        disc_number,
        cover_art,
        rating,
        musical_key,
    ) = match tag {
        Some(tag) => (
            tag.title().map(|s| s.into_owned()),
            tag.artist().map(|s| s.into_owned()),
            tag.album().map(|s| s.into_owned()),
            extract_album_artist(tag),
            extract_compilation_flag(tag),
            tag.genre().map(|s| s.into_owned()),
            tag.date().map(|d| d.year as i64),
            tag.track().map(|n| n as i64),
            tag.disk().map(|n| n as i64),
            extract_cover(tag, artwork_dir),
            extract_rating(tag),
            extract_musical_key(tag),
        ),
        None => (
            None, None, None, None, false, None, None, None, None, None, None, None,
        ),
    };

    // Folder cover fallback: scan the track's parent directory for a
    // sidecar cover.jpg / folder.png / front.webp / ... when the tag had
    // no embedded picture. Common for CD rips and lossless libraries
    // where the artwork lives next to the audio files.
    let cover_art = cover_art.or_else(|| extract_folder_cover(path, artwork_dir));

    // Fall back to the file stem when the tag has no title — better than
    // displaying an empty string in the library grid.
    let title = title.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string()
    });

    Ok(ExtractedFile {
        abs_path: path.to_string_lossy().to_string(),
        size,
        modified_ms,
        hash,
        title,
        artist,
        album,
        album_artist,
        is_compilation,
        genre,
        year,
        track_number,
        disc_number,
        duration_ms,
        bitrate,
        sample_rate,
        channels,
        bit_depth,
        codec,
        musical_key,
        cover_art,
        rating,
    })
}

/// Walk an existing `library_folder` on disk, extract tags from every audio
/// file, and upsert them into the active profile's database.
///
/// New files are inserted, existing rows are updated in place (keying on
/// `(library_id, file_path)`), and files that haven't changed since the last
/// scan — matched on `(file_modified, file_hash)` — are skipped to keep the
/// loop fast on re-scans.
///
/// Failures on individual files are logged but never abort the scan: the
/// summary counter `errors` surfaces them to the UI so the user can tell how
/// many files were rejected.
#[tauri::command]
pub async fn scan_folder(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    folder_id: i64,
) -> AppResult<ScanSummary> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let summary = scan_folder_inner(&pool, &artwork_dir, folder_id, Some(&app)).await?;
    // Phase 4.d.0.3: wake the sync drain so the freshly enqueued
    // track ops ship immediately instead of waiting on the
    // drain's idle poll. Matches the convention every other
    // CRUD command in this crate already follows
    // (`commands/library.rs`, `commands/playlist.rs`).
    state.drain.notify();
    // Fire the auto-analyzer in the background when the user has
    // opted in. Spawned so the IPC reply doesn't block on a
    // potentially long analysis pass.
    if summary.added > 0 {
        crate::commands::analysis::maybe_auto_analyze(&app);
    }
    Ok(summary)
}

/// Inner scan implementation shared between the `scan_folder` command and
/// the `rescan_library` command, which walks every folder of a library.
///
/// Takes the resolved database pool + artwork directory directly so it can
/// run in contexts where a `tauri::State` isn't available (e.g. called in a
/// loop from another command).
pub(crate) async fn scan_folder_inner(
    pool: &SqlitePool,
    artwork_dir: &Path,
    folder_id: i64,
    app_handle: Option<&tauri::AppHandle>,
) -> AppResult<ScanSummary> {
    // Mark a scan in flight for the whole body so the background
    // analyzer parks itself instead of contending on CPU + the single
    // SQLite writer. Dropped on every exit path (RAII).
    let _scan_guard = ScanInFlightGuard::new();

    // Belt-and-braces: the directory is created at profile bootstrap, but a
    // user fiddling with the data folder could have deleted it.
    std::fs::create_dir_all(artwork_dir)?;

    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT library_id, path FROM library_folder WHERE id = ?")
            .bind(folder_id)
            .fetch_optional(pool)
            .await?;
    let Some((library_id, folder_path)) = row else {
        return Err(AppError::Other(format!("folder {folder_id} not found")));
    };

    // Phase timers (diagnostics — logged once at the end so a slow scan
    // on a big library tells us which phase to optimise).
    let t_scan = Instant::now();

    // Walk the directory off-thread — walkdir is blocking and a deep tree can
    // take a noticeable fraction of a second to enumerate.
    let folder_path_owned = folder_path.clone();
    let audio_files: Vec<PathBuf> = tokio::task::spawn_blocking(move || {
        WalkDir::new(&folder_path_owned)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| AUDIO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
                    .unwrap_or(false)
            })
            .map(|entry| entry.path().to_path_buf())
            .collect()
    })
    .await
    .map_err(|e| AppError::Other(format!("walk task failed: {e}")))?;
    let walk_ms = t_scan.elapsed().as_millis();

    let mut summary = ScanSummary {
        folder_id,
        ..Default::default()
    };
    let now = now_millis();

    // Snapshot of (path → (file_modified_ms, file_size)) for every
    // currently-available track in this folder.
    //
    // Two purposes:
    //   1. Re-scan fast path — if the file on disk still has the same
    //      mtime + size as the row, we skip the expensive `extract_file`
    //      (hash + tag re-read) entirely. Re-scans of an 800-track
    //      library drop from ~30 s to <1 s.
    //   2. Disappearance sweep — paths still in the map at the end were
    //      on disk last time but aren't now, so we mark them unavailable
    //      below.
    //
    // Tracks already at `is_available = 0` are excluded — bringing them
    // back is handled by the upsert path which re-sets the flag to 1.
    let existing_rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT file_path, file_modified, file_size
           FROM track
          WHERE folder_id = ? AND is_available = 1",
    )
    .bind(folder_id)
    .fetch_all(pool)
    .await?;
    let mut existing_meta: HashMap<String, (i64, i64)> = existing_rows
        .into_iter()
        .map(|(p, mtime, size)| (p, (mtime, size)))
        .collect();

    // Probe map for the insert-vs-update decision in the consumer loop,
    // pre-loaded ONCE instead of a `SELECT … WHERE library_id = ? AND
    // file_path = ?` per extracted track (the per-track probe was a
    // wasted round-trip on every brand-new file — always a miss — and
    // those dominate a first scan). Keyed library-wide, NOT folder-
    // scoped like `existing_meta`: a file already known under another
    // folder of the same library must still resolve so the loop UPDATEs
    // (and reassigns `folder_id`) instead of hitting the
    // `UNIQUE(library_id, file_path)` constraint. No `is_available`
    // filter — matches the old probe, so a vanished-then-restored track
    // is found and flipped back to available. Snapshot-before-loop is
    // safe: the walk yields distinct paths, so no row inserted earlier
    // in THIS scan is ever probed later.
    let probe_rows: Vec<(String, i64, i64, String, i64)> = sqlx::query_as(
        "SELECT file_path, id, file_modified, file_hash, added_at
           FROM track
          WHERE library_id = ?",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await?;
    let existing_probe: HashMap<String, (i64, i64, String, i64)> = probe_rows
        .into_iter()
        .map(|(path, id, mtime, hash, added_at)| (path, (id, mtime, hash, added_at)))
        .collect();

    let meta_load_ms = t_scan.elapsed().as_millis();

    let total_files = audio_files.len();

    // Initial tick so the frontend's progress toast can size itself
    // even before the first file is processed (helps when the loop is
    // mostly skips and would otherwise emit nothing for several
    // hundred files).
    if let Some(app) = app_handle {
        let _ = app.emit(
            "scan:progress",
            ScanProgress {
                folder_id,
                current: 0,
                total: total_files,
                added: 0,
                updated: 0,
                skipped: 0,
                errors: 0,
                done: false,
            },
        );
    }

    // ─── Phase 1: Serial fast-path classification ─────────────────
    // Cheap fs::metadata (one syscall per file) lets us short-circuit
    // re-scans where nothing changed. Files that survive are pushed
    // to `to_extract` for the parallel extraction phase.
    let mut to_extract: Vec<PathBuf> = Vec::with_capacity(audio_files.len());
    for (idx, path) in audio_files.into_iter().enumerate() {
        summary.scanned += 1;
        let path_str = path.to_string_lossy().into_owned();
        if let Some((stored_mtime, stored_size)) = existing_meta.get(&path_str) {
            if let Ok(metadata) = std::fs::metadata(&path) {
                let disk_size = metadata.len() as i64;
                let disk_mtime_ms = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                if disk_size == *stored_size && disk_mtime_ms == *stored_mtime {
                    existing_meta.remove(&path_str);
                    summary.skipped += 1;
                    maybe_emit_progress(app_handle, folder_id, idx + 1, total_files, &summary);
                    continue;
                }
            }
        }
        to_extract.push(path);
    }
    let stat_ms = t_scan.elapsed().as_millis();
    let to_extract_count = to_extract.len();

    // ─── Phase 2: Parallel extract + transactional DB writes ──────
    //
    // `extract_file` is CPU-bound (BLAKE3 hash + lofty tag read) and
    // I/O-bound (file open + parent-dir walk for cover sidecar).
    // Spawn N extractions in flight via `buffered`, where N is the
    // host's parallelism count — saturates a multi-core CPU on the
    // hash without overwhelming the kernel I/O scheduler.
    //
    // DB writes stay serial (one writer per SQLite WAL is a hard
    // constraint anyway) but ride inside a transaction that commits
    // every TX_BATCH rows. With synchronous=NORMAL + WAL, this drops
    // a 1k-track first scan from ~30 s to a few seconds — fsync per
    // checkpoint instead of per row.
    let parallelism = std::thread::available_parallelism()
        .map(|n| n.get().max(2))
        .unwrap_or(4);
    const TX_BATCH: usize = 200;

    let timings = Arc::new(ScanTimings::default());
    let extraction_stream = futures::stream::iter(to_extract)
        .map(|path: PathBuf| {
            let artwork_dir = artwork_dir.to_path_buf();
            let timings = Arc::clone(&timings);
            let p = path.clone();
            async move {
                let res =
                    tokio::task::spawn_blocking(move || extract_file(&p, &artwork_dir, &timings))
                        .await;
                (path, res)
            }
        })
        .buffered(parallelism);
    futures::pin_mut!(extraction_stream);

    let mut tx = pool.begin().await?;
    let mut tx_count: usize = 0;
    let mut processed: usize = summary.skipped as usize;
    // Memo for artist / genre id lookups across the whole scan — see
    // `UpsertCache`. Lives for the loop's duration so every track on
    // the same album / by the same artist reuses the first resolution.
    let mut upsert_cache = UpsertCache::default();

    while let Some((path, result)) = extraction_stream.next().await {
        processed += 1;
        let extracted = match result {
            Ok(Ok(e)) => e,
            Ok(Err(err)) => {
                tracing::warn!(path = %path.display(), error = %err, "extraction failed");
                summary.errors += 1;
                continue;
            }
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "extraction panicked");
                summary.errors += 1;
                continue;
            }
        };

        // File is on disk → keep it out of the deletion sweep below.
        existing_meta.remove(&extracted.abs_path);

        // Time the serial DB work for this track (probe + upserts +
        // INSERT/UPDATE + the periodic commit). `existing_meta.remove`
        // above is an in-memory HashMap op, negligible. No `continue`
        // sits between here and the accumulate below, so one delta
        // covers the whole iteration's DB cost.
        let t_db = Instant::now();

        // Insert-vs-update probe, served from the pre-loaded
        // `existing_probe` map instead of a per-track SELECT. Carries
        // `added_at` so we preserve the row's original "first import"
        // timestamp on the wire — re-emits must NOT bump it (a peer
        // device receiving a re-emit op for an existing track would
        // otherwise see the file as "just added" in its `Recently
        // added` view). Brand-new track path keeps using `now`.
        let existing = existing_probe.get(&extracted.abs_path).cloned();

        if let Some((existing_track_id, mtime, ref hash, existing_added_at)) = existing {
            if mtime == extracted.modified_ms && hash == &extracted.hash {
                // Track content hasn't changed — backfill paths only.
                // See the historical comments at the top of this branch
                // for context on why these queries still run on a
                // hash-match (cover backfill, codec/key backfill,
                // multi-artist normalisation).
                if let Some(cover) = &extracted.cover_art {
                    let row: Option<(Option<i64>, Option<i64>)> = sqlx::query_as(
                        "SELECT t.album_id, al.artwork_id
                           FROM track t
                           LEFT JOIN album al ON al.id = t.album_id
                          WHERE t.id = ?",
                    )
                    .bind(existing_track_id)
                    .fetch_optional(&mut *tx)
                    .await?;
                    if let Some((Some(aid), None)) = row {
                        let artwork_id =
                            upsert_artwork(&mut tx, &cover.hash, &cover.format, cover.source)
                                .await?;
                        sqlx::query("UPDATE album SET artwork_id = ? WHERE id = ?")
                            .bind(artwork_id)
                            .bind(aid)
                            .execute(&mut *tx)
                            .await?;
                    }
                }

                sqlx::query(
                    "UPDATE track
                        SET bit_depth   = COALESCE(bit_depth, ?),
                            codec       = COALESCE(codec, ?),
                            musical_key = COALESCE(musical_key, ?)
                      WHERE id = ?",
                )
                .bind(extracted.bit_depth)
                .bind(extracted.codec.as_deref())
                .bind(extracted.musical_key.as_deref())
                .bind(existing_track_id)
                .execute(&mut *tx)
                .await?;

                let mut multi_artist_renormalised = false;
                if let Some(raw) = &extracted.artist {
                    let splits = split_artist_name(raw);
                    let current_count: i64 =
                        sqlx::query_scalar("SELECT COUNT(*) FROM track_artist WHERE track_id = ?")
                            .bind(existing_track_id)
                            .fetch_one(&mut *tx)
                            .await?;
                    if current_count as usize != splits.len() {
                        multi_artist_renormalised = true;
                        let mut ids = Vec::new();
                        for name in splits {
                            if let Some(id) = upsert_artist(&mut tx, &name).await? {
                                ids.push(id);
                            }
                        }
                        sqlx::query("DELETE FROM track_artist WHERE track_id = ?")
                            .bind(existing_track_id)
                            .execute(&mut *tx)
                            .await?;
                        for (position, aid) in ids.iter().enumerate() {
                            sqlx::query(
                                "INSERT INTO track_artist (track_id, artist_id, role, position)
                                 VALUES (?, ?, 'main', ?)",
                            )
                            .bind(existing_track_id)
                            .bind(aid)
                            .bind(position as i64)
                            .execute(&mut *tx)
                            .await?;
                        }
                        sqlx::query("UPDATE track SET primary_artist = ? WHERE id = ?")
                            .bind(ids.first().copied())
                            .bind(existing_track_id)
                            .execute(&mut *tx)
                            .await?;
                        if let Some(first_id) = ids.first().copied() {
                            sqlx::query(
                                "UPDATE album SET artist_id = ?
                                 WHERE id = (SELECT album_id FROM track WHERE id = ?)
                                   AND artist_id != ?",
                            )
                            .bind(first_id)
                            .bind(existing_track_id)
                            .bind(first_id)
                            .execute(&mut *tx)
                            .await?;
                        }
                    }
                    // Backfill local artist images AFTER the optional
                    // track_artist rebuild — otherwise newly created
                    // artist IDs (when current_count != splits.len())
                    // would be skipped on first encounter. Cheap because
                    // already-linked artists are filtered by the
                    // `IS NOT NULL` pre-check inside the helper.
                    let track_path = Path::new(&extracted.abs_path);
                    let current_ids: Vec<i64> = sqlx::query_scalar(
                        "SELECT artist_id FROM track_artist
                          WHERE track_id = ? ORDER BY position",
                    )
                    .bind(existing_track_id)
                    .fetch_all(&mut *tx)
                    .await?;
                    maybe_link_artist_images(
                        &mut tx,
                        Some(raw),
                        &current_ids,
                        track_path,
                        artwork_dir,
                    )
                    .await?;
                }

                // Phase 4.d.0.3: emit a sync op ONLY when the skip
                // branch actually rewrote multi-artist rows
                // (`track_artist` re-link from comma-joined → ";"-
                // split). Without this, a peer device that pulls
                // down the original track via sync would never
                // observe the multi-artist normalisation. Cover
                // and codec backfills don't need to round-trip
                // because they're server-derived metadata; only
                // the multi-artist relink is wire-visible.
                if multi_artist_renormalised {
                    emit_track_insert_from_extracted(
                        &mut tx,
                        library_id,
                        existing_track_id,
                        &extracted,
                        existing_added_at,
                    )
                    .await?;
                }

                summary.skipped += 1;
                tx_count += 1;
            } else {
                // Hash or mtime changed → full re-write of the track row
                // and its many-to-many links. Falls through to the
                // shared insert/update path below.
                let artist_ids = upsert_cache.artist_list(&mut tx, &extracted.artist).await?;
                let artist_id = artist_ids.first().copied();
                let album_id = match &extracted.album {
                    Some(a) => {
                        upsert_album(
                            &mut tx,
                            a,
                            extracted.album_artist.as_deref(),
                            extracted.is_compilation,
                            artist_id,
                            extracted.year,
                        )
                        .await?
                    }
                    None => None,
                };
                let genre_id = match &extracted.genre {
                    Some(g) => upsert_cache.genre(&mut tx, g).await?,
                    None => None,
                };
                if let (Some(cover), Some(aid)) = (&extracted.cover_art, album_id) {
                    let artwork_id =
                        upsert_artwork(&mut tx, &cover.hash, &cover.format, cover.source).await?;
                    sqlx::query(
                        "UPDATE album SET artwork_id = ? WHERE id = ? AND artwork_id IS NULL",
                    )
                    .bind(artwork_id)
                    .bind(aid)
                    .execute(&mut *tx)
                    .await?;
                }

                maybe_link_artist_images(
                    &mut tx,
                    extracted.artist.as_deref(),
                    &artist_ids,
                    Path::new(&extracted.abs_path),
                    artwork_dir,
                )
                .await?;

                sqlx::query(
                    "UPDATE track SET
                        folder_id = ?,
                        file_hash = ?, file_size = ?, file_modified = ?,
                        title = ?, album_id = ?, primary_artist = ?,
                        track_number = ?, disc_number = ?, year = ?,
                        duration_ms = ?, bitrate = ?, sample_rate = ?, channels = ?,
                        bit_depth = ?, codec = ?,
                        musical_key = ?,
                        rating = ?,
                        is_available = 1
                     WHERE id = ?",
                )
                .bind(folder_id)
                .bind(&extracted.hash)
                .bind(extracted.size)
                .bind(extracted.modified_ms)
                .bind(&extracted.title)
                .bind(album_id)
                .bind(artist_id)
                .bind(extracted.track_number)
                .bind(extracted.disc_number)
                .bind(extracted.year)
                .bind(extracted.duration_ms)
                .bind(extracted.bitrate)
                .bind(extracted.sample_rate)
                .bind(extracted.channels)
                .bind(extracted.bit_depth)
                .bind(extracted.codec.as_deref())
                .bind(extracted.musical_key.as_deref())
                .bind(extracted.rating.map(|r| r as i64))
                .bind(existing_track_id)
                .execute(&mut *tx)
                .await?;

                sqlx::query("DELETE FROM track_artist WHERE track_id = ?")
                    .bind(existing_track_id)
                    .execute(&mut *tx)
                    .await?;
                for (position, aid) in artist_ids.iter().enumerate() {
                    sqlx::query(
                        "INSERT INTO track_artist (track_id, artist_id, role, position)
                         VALUES (?, ?, 'main', ?)",
                    )
                    .bind(existing_track_id)
                    .bind(aid)
                    .bind(position as i64)
                    .execute(&mut *tx)
                    .await?;
                }

                sqlx::query("DELETE FROM track_genre WHERE track_id = ?")
                    .bind(existing_track_id)
                    .execute(&mut *tx)
                    .await?;
                if let Some(gid) = genre_id {
                    sqlx::query("INSERT INTO track_genre (track_id, genre_id) VALUES (?, ?)")
                        .bind(existing_track_id)
                        .bind(gid)
                        .execute(&mut *tx)
                        .await?;
                }

                // Phase 4.d.0.3: emit the track insert op so the
                // server's apply pipeline mirrors the update. The
                // upsert merges every scalar — wire-shape-identical
                // to the brand-new track branch below. Skipped
                // gracefully when sync isn't configured (no JWT /
                // Local mode).
                //
                // `added_at` carries the row's ORIGINAL import
                // timestamp, not `now` — a re-emit must not bump
                // the peer's `Recently added` ordering.
                emit_track_insert_from_extracted(
                    &mut tx,
                    library_id,
                    existing_track_id,
                    &extracted,
                    existing_added_at,
                )
                .await?;

                summary.updated += 1;
                tx_count += 1;
            }
        } else {
            // Brand-new track — insert with all related upserts.
            let artist_ids = upsert_cache.artist_list(&mut tx, &extracted.artist).await?;
            let artist_id = artist_ids.first().copied();
            let album_id = match &extracted.album {
                Some(a) => {
                    upsert_album(
                        &mut tx,
                        a,
                        extracted.album_artist.as_deref(),
                        extracted.is_compilation,
                        artist_id,
                        extracted.year,
                    )
                    .await?
                }
                None => None,
            };
            let genre_id = match &extracted.genre {
                Some(g) => upsert_cache.genre(&mut tx, g).await?,
                None => None,
            };
            if let (Some(cover), Some(aid)) = (&extracted.cover_art, album_id) {
                let artwork_id =
                    upsert_artwork(&mut tx, &cover.hash, &cover.format, cover.source).await?;
                sqlx::query("UPDATE album SET artwork_id = ? WHERE id = ? AND artwork_id IS NULL")
                    .bind(artwork_id)
                    .bind(aid)
                    .execute(&mut *tx)
                    .await?;
            }

            maybe_link_artist_images(
                &mut tx,
                extracted.artist.as_deref(),
                &artist_ids,
                Path::new(&extracted.abs_path),
                artwork_dir,
            )
            .await?;

            let insert = sqlx::query(
                "INSERT INTO track (
                    library_id, folder_id, file_path, file_hash, file_size, file_modified,
                    title, album_id, primary_artist,
                    track_number, disc_number, year,
                    duration_ms, bitrate, sample_rate, channels,
                    bit_depth, codec, musical_key,
                    rating,
                    added_at, is_available
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)",
            )
            .bind(library_id)
            .bind(folder_id)
            .bind(&extracted.abs_path)
            .bind(&extracted.hash)
            .bind(extracted.size)
            .bind(extracted.modified_ms)
            .bind(&extracted.title)
            .bind(album_id)
            .bind(artist_id)
            .bind(extracted.track_number)
            .bind(extracted.disc_number)
            .bind(extracted.year)
            .bind(extracted.duration_ms)
            .bind(extracted.bitrate)
            .bind(extracted.sample_rate)
            .bind(extracted.channels)
            .bind(extracted.bit_depth)
            .bind(extracted.codec.as_deref())
            .bind(extracted.musical_key.as_deref())
            .bind(extracted.rating.map(|r| r as i64))
            .bind(now)
            .execute(&mut *tx)
            .await?;
            let track_id = insert.last_insert_rowid();

            for (position, aid) in artist_ids.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO track_artist (track_id, artist_id, role, position)
                     VALUES (?, ?, 'main', ?)",
                )
                .bind(track_id)
                .bind(aid)
                .bind(position as i64)
                .execute(&mut *tx)
                .await?;
            }
            if let Some(gid) = genre_id {
                sqlx::query("INSERT INTO track_genre (track_id, genre_id) VALUES (?, ?)")
                    .bind(track_id)
                    .bind(gid)
                    .execute(&mut *tx)
                    .await?;
            }

            // Phase 4.d.0.3: emit the track insert op for the
            // sync server. Sits inside the same tx as the entity
            // write — outbox rolls back with the track row if the
            // commit fails. Skipped gracefully when sync isn't
            // configured.
            emit_track_insert_from_extracted(&mut tx, library_id, track_id, &extracted, now)
                .await?;

            summary.added += 1;
            tx_count += 1;
        }

        // Periodic commit so the WAL doesn't grow unbounded on big
        // first scans, AND so a failure mid-scan loses at most
        // TX_BATCH rows of work instead of the whole import.
        if tx_count >= TX_BATCH {
            tx.commit().await?;
            tx = pool.begin().await?;
            tx_count = 0;
        }

        timings
            .db_us
            .fetch_add(t_db.elapsed().as_micros() as u64, Ordering::Relaxed);

        maybe_emit_progress(app_handle, folder_id, processed, total_files, &summary);
    }

    tx.commit().await?;
    let extract_db_ms = t_scan.elapsed().as_millis();

    // Anything still in the map was on disk last time but isn't now.
    // Mark it unavailable rather than deleting — preserves play_event
    // history and lets the user "undelete" by restoring the file.
    // SQLite caps bound parameters at ~999, so we update one row at a
    // time. Removed counts are normally tiny (a handful per scan); for
    // bulk wipes the loop is still acceptable since we're already
    // off the audio thread.
    for missing_path in existing_meta.keys() {
        let res = sqlx::query(
            "UPDATE track SET is_available = 0
              WHERE folder_id = ? AND file_path = ? AND is_available = 1",
        )
        .bind(folder_id)
        .bind(missing_path)
        .execute(pool)
        .await?;
        if res.rows_affected() > 0 {
            summary.removed += 1;
        }
    }

    sqlx::query("UPDATE library_folder SET last_scanned_at = ? WHERE id = ?")
        .bind(now)
        .bind(folder_id)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE library SET updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(library_id)
        .execute(pool)
        .await?;

    // Auto-detect compilations among the tagless rows. Catches the case
    // where a Various-Artists record has neither aART nor TCMP set on
    // its source files (Soothing-Breeze-style lofi compilations,
    // SoundCloud DL packs, …) — the scanner would otherwise leave 21
    // single-track album rows fragmented by primary_artist.
    if let Err(err) = merge_implicit_compilations(pool).await {
        tracing::warn!(?err, "merge_implicit_compilations failed (non-fatal)");
    }

    // VA is an album artist (never in `track_artist`), so the per-track
    // `maybe_link_artist_images` pass above can't reach it — resolve a
    // curated `Various Artists/artist.jpg` via the album relationship here
    // (issue #292). Runs after the merge so a freshly promoted VA row is
    // covered. Non-fatal: a missing sidecar is the common case.
    match pool.acquire().await {
        Ok(mut conn) => {
            if let Err(err) = link_va_artist_image(&mut conn, artwork_dir).await {
                tracing::warn!(?err, "link_va_artist_image failed (non-fatal)");
            }
        }
        Err(err) => tracing::warn!(?err, "link_va_artist_image: acquire failed (non-fatal)"),
    }

    // Per-phase breakdown so a slow scan on a big library is diagnosable
    // from the log alone. `*_ms` are wall-clock deltas between phases;
    // `hash_cpu_ms_total` / `tag_cpu_ms_total` are summed across the
    // `parallelism` extraction threads, so compare them to
    // `extract_db_ms * parallelism` to tell hash-bound from tag-bound.
    tracing::info!(
        folder_id,
        library_id,
        scanned = summary.scanned,
        added = summary.added,
        updated = summary.updated,
        skipped = summary.skipped,
        removed = summary.removed,
        errors = summary.errors,
        extracted = to_extract_count,
        parallelism,
        walk_ms,
        meta_load_ms = meta_load_ms.saturating_sub(walk_ms),
        stat_ms = stat_ms.saturating_sub(meta_load_ms),
        extract_db_ms = extract_db_ms.saturating_sub(stat_ms),
        post_ms = t_scan.elapsed().as_millis().saturating_sub(extract_db_ms),
        total_ms = t_scan.elapsed().as_millis(),
        hash_cpu_ms_total = timings.hash_us.load(Ordering::Relaxed) / 1000,
        tag_cpu_ms_total = timings.tag_us.load(Ordering::Relaxed) / 1000,
        // Serial single-writer DB time — already wall-clock (one
        // consumer task). The slice of `extract_db_ms` NOT covered by
        // this is the parallel extraction (hash + cover) the consumer
        // waited on.
        db_ms_total = timings.db_us.load(Ordering::Relaxed) / 1000,
        "scan complete"
    );

    if let Some(app) = app_handle {
        let _ = app.emit(
            "scan:progress",
            ScanProgress {
                folder_id,
                current: total_files,
                total: total_files,
                added: summary.added,
                updated: summary.updated,
                skipped: summary.skipped,
                errors: summary.errors,
                done: true,
            },
        );
    }

    Ok(summary)
}

/// Summary returned by [`rescan_local_artist_images`].
#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistImageScanSummary {
    /// Number of artists checked (those without an existing `artwork_id`).
    pub considered: i64,
    /// Number of artists that now have a local sidecar image linked.
    pub linked: i64,
}

/// Walk every `artist` row that has no `artwork_id` and try to resolve a
/// sidecar image from any of their tracks' folders. Cheap on re-runs
/// because already-linked rows are excluded by the SQL filter and we
/// stop at the first track that yields a match.
///
/// Lets users who scanned their library before this feature shipped pick
/// up `artist.jpg` files without re-importing every folder.
#[tauri::command]
pub async fn rescan_local_artist_images(
    state: tauri::State<'_, AppState>,
) -> AppResult<ArtistImageScanSummary> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    std::fs::create_dir_all(&artwork_dir)?;

    let va_canon = canonical_name(VARIOUS_ARTISTS_LABEL);
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT id, name, canonical_name FROM artist
          WHERE artwork_id IS NULL
            AND canonical_name != ?",
    )
    .bind(&va_canon)
    .fetch_all(&pool)
    .await?;

    let mut summary = ArtistImageScanSummary {
        considered: rows.len() as i64,
        linked: 0,
    };

    // Batch writes through a single transaction (committed every
    // TX_BATCH writes) so SQLite WAL fsyncs once per batch instead of
    // once per artist — same pattern as scan_folder_inner.
    const TX_BATCH: usize = 200;
    let mut tx = pool.begin().await?;
    let mut tx_count: usize = 0;

    for (artist_id, _name, canon) in rows {
        // Track lookup is a read — run it on the pool so it doesn't
        // serialise behind the open write transaction.
        let tracks: Vec<(String,)> = sqlx::query_as(
            "SELECT t.file_path FROM track t
               JOIN track_artist ta ON ta.track_id = t.id
              WHERE ta.artist_id = ? AND t.is_available = 1
              LIMIT 16",
        )
        .bind(artist_id)
        .fetch_all(&pool)
        .await?;

        let mut linked = false;
        for (path,) in tracks {
            if let Some(cover) = extract_artist_image(Path::new(&path), &canon, &artwork_dir) {
                link_local_artist_image(&mut tx, artist_id, &cover).await?;
                linked = true;
                break;
            }
        }
        if linked {
            summary.linked += 1;
            tx_count += 1;
            if tx_count >= TX_BATCH {
                tx.commit().await?;
                tx = pool.begin().await?;
                tx_count = 0;
            }
        }
    }

    // The main loop joins `track_artist`, which the "Various Artists"
    // sentinel never appears in (it's an album artist) — resolve its
    // sidecar via the album relationship instead (issue #292). The
    // rows query above already excluded VA via `canonical_name != ?`.
    if let Some(linked) = link_va_artist_image(&mut tx, &artwork_dir).await? {
        summary.considered += 1;
        if linked {
            summary.linked += 1;
        }
    }

    tx.commit().await?;

    tracing::info!(
        considered = summary.considered,
        linked = summary.linked,
        "rescan_local_artist_images complete",
    );
    Ok(summary)
}

/// Sync-emit shim — converts an `ExtractedFile` into the wire shape
/// the server's `apply::track::insert` handler expects (phase
/// 4.d.0.2) and enqueues the op via the standard outbox path.
///
/// Inlined here rather than in `sync::track_emit` because the
/// scanner is the only call site that operates on
/// `ExtractedFile`; the lower-level helper takes a borrowed
/// `TrackInsertWire` so other call sites (duplicates UI, future
/// importers) build it from their own context.
async fn emit_track_insert_from_extracted(
    tx: &mut sqlx::SqliteConnection,
    library_id: i64,
    track_id: i64,
    extracted: &ExtractedFile,
    added_at: i64,
) -> AppResult<()> {
    // The scanner's `; `-split convention drives the wire-shape
    // `artists` array. Position derives from the index after the
    // split, matching the server's `track_artist.position`
    // semantic.
    let artists: Vec<String> = match &extracted.artist {
        Some(s) => split_artist_name(s),
        None => Vec::new(),
    };
    let wire = crate::sync::track_emit::TrackInsertWire {
        file_hash: &extracted.hash,
        title: &extracted.title,
        file_size: extracted.size,
        file_modified: extracted.modified_ms,
        duration_ms: extracted.duration_ms,
        track_number: extracted.track_number,
        disc_number: extracted.disc_number,
        year: extracted.year,
        bitrate: extracted.bitrate,
        sample_rate: extracted.sample_rate,
        channels: extracted.channels,
        bit_depth: extracted.bit_depth,
        codec: extracted.codec.as_deref(),
        musical_key: extracted.musical_key.as_deref(),
        added_at,
        album_title: extracted.album.as_deref(),
        album_artist_name: extracted.album_artist.as_deref(),
        is_compilation: extracted.is_compilation,
        artists: &artists,
    };
    crate::sync::track_emit::emit_track_insert_in_tx(
        tx,
        library_id,
        track_id,
        &extracted.abs_path,
        &wire,
    )
    .await?;
    Ok(())
}

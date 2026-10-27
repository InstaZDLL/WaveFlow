//! Files played without importing them.
//!
//! A file opened from the system (double-click, "Open with", the command
//! line) or from the app's own open dialog plays without becoming a library
//! track: nothing is written to the database, it counts in no history, no
//! statistics and no scrobble, the way a radio stream does. Its queue lives
//! in memory beside the library queue, on the same [`DetachedSession`] the
//! remote queue uses, so it takes over next / previous and the end of a
//! track while it is active, and the ordering rules of #622 hold for it.
//!
//! A file that already is a library track is played as one: opening it
//! from the file manager should not lose its likes, its rating or its
//! history. That holds when **every** file opened is in the library;
//! a mix plays them all as files, rather than split one open across two
//! queues.
//!
//! The tags of a file are read when it plays, not when it is queued:
//! opening a folder of a thousand files reads one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::{AppHandle, Manager};
use waveflow_core::playback::replay_gain::TrackGain;

use crate::audio::engine::{DetachedOrigin, LoadIntent};
use crate::audio::{AudioCmd, AudioEngine};
use crate::error::{AppError, AppResult};
use crate::queue::Direction;
use crate::remote_playback::{DetachedQueue, DetachedSession};
use crate::state::AppState;

/// One file of the queue. Its tags are read when it plays.
#[derive(Debug, Clone)]
pub struct ExternalEntry {
    pub path: PathBuf,
}

/// The session of files played without importing them.
pub type ExternalPlayback = DetachedSession<ExternalEntry>;

/// Most files one open queues.
const MAX_FILES: usize = 5_000;

/// Most entries one open walks, audio or not: a folder dropped by mistake
/// on a whole drive should not walk it to the end, even one with no audio
/// file to reach [`MAX_FILES`] with.
const MAX_VISITED: usize = 200_000;

/// The audio files `paths` name, in order: a file as it is, a folder as the
/// audio files under it, sorted by name, its `.waveflow` branch skipped as
/// the scanner skips it. Anything else is passed over. Blocking.
pub fn expand(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut visited = 0usize;
    for path in paths {
        if files.len() >= MAX_FILES || visited >= MAX_VISITED {
            break;
        }
        if path.is_dir() {
            let walk = walkdir::WalkDir::new(path)
                .sort_by(|a, b| a.file_name().cmp(b.file_name()))
                .into_iter()
                .filter_entry(|entry| {
                    entry.depth() == 0
                        || !entry.file_type().is_dir()
                        || !waveflow_core::scanner::is_reserved_dir_name(entry.file_name())
                });
            for entry in walk.flatten() {
                visited += 1;
                if visited >= MAX_VISITED {
                    break;
                }
                // A link to a file counts as the file; a link to a folder
                // is not followed, which keeps the walk out of loops.
                let is_file = entry.file_type().is_file()
                    || (entry.path_is_symlink() && entry.path().is_file());
                if is_file && waveflow_core::scanner::extract::is_scannable_audio(entry.path()) {
                    files.push(entry.into_path());
                    if files.len() >= MAX_FILES {
                        break;
                    }
                }
            }
        } else if path.is_file() && waveflow_core::scanner::extract::is_scannable_audio(path) {
            files.push(path.clone());
        }
    }
    files
}

/// What the player shows for a file, read from it.
#[derive(Debug, Clone, PartialEq)]
struct FileFacts {
    title: String,
    artist: Option<String>,
    duration_ms: u64,
    /// The cover, written into `artwork_dir` under its hash, the way the
    /// scanner writes one.
    cover: Option<PathBuf>,
    replay_gain: TrackGain,
}

/// Read the tags, the length and the cover of `path`, without the
/// database. The title falls back to the file's name, so a file with no
/// tags still reads as something. Blocking.
fn read_facts(path: &Path, artwork_dir: &Path) -> FileFacts {
    use waveflow_core::scanner::extract;

    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cover_path = |cover: extract::ExtractedCover| {
        artwork_dir.join(format!("{}.{}", cover.hash, cover.format))
    };
    let folder_cover = || extract::extract_folder_cover(path, artwork_dir).map(cover_path);
    let mut facts = FileFacts {
        title: name,
        artist: None,
        duration_ms: 0,
        cover: None,
        replay_gain: TrackGain::default(),
    };

    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase);
    if let Some(container) = match extension.as_deref() {
        Some("dsf") => Some(waveflow_core::audio_format::dsd::parser::DsdContainer::Dsf),
        Some("dff") => Some(waveflow_core::audio_format::dsd::parser::DsdContainer::Dff),
        _ => None,
    } {
        if let Ok(mut file) = std::fs::File::open(path) {
            use waveflow_core::audio_format::dsd::{metadata, parser};
            let layout = match container {
                parser::DsdContainer::Dsf => parser::parse_dsf(&mut file),
                parser::DsdContainer::Dff => parser::parse_dff(&mut file),
            };
            if let Ok(layout) = layout {
                facts.duration_ms = layout.duration_ms();
            }
            if let Ok(tags) = metadata::read_metadata(&mut file, container) {
                if let Some(title) = tags.title.filter(|t| !t.trim().is_empty()) {
                    facts.title = title;
                }
                facts.artist = tags.artist;
            }
        }
        facts.cover = folder_cover();
        return facts;
    }

    match extract::read_tagged_file(path) {
        Ok(tagged) => {
            use lofty::file::TaggedFileExt;
            use lofty::prelude::{Accessor, AudioFile};
            facts.duration_ms = tagged.properties().duration().as_millis() as u64;
            if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
                if let Some(title) = tag.title().filter(|t| !t.trim().is_empty()) {
                    facts.title = title.into_owned();
                }
                facts.artist = tag.artist().map(|a| a.into_owned());
                facts.cover = extract::extract_cover(tag, artwork_dir).map(cover_path);
                let gain = waveflow_core::scanner::replay_gain::extract_replay_gain(tag);
                facts.replay_gain = TrackGain {
                    gain_db: gain.track_gain_db,
                    peak: gain.track_peak,
                    album_gain_db: gain.album_gain_db,
                    album_peak: gain.album_peak,
                    ..TrackGain::default()
                };
            }
            if facts.cover.is_none() {
                facts.cover = folder_cover();
            }
        }
        // Tags nobody can read do not stop a file the decoder can play.
        Err(_) => {
            if let Some(stream) = extract::probe_playable_stream(path) {
                facts.duration_ms = stream.duration_ms.max(0) as u64;
            }
            facts.cover = folder_cover();
        }
    }
    facts
}

/// The library ids of `files`, in order, when every one of them is an
/// available library track; `None` as soon as one is not.
async fn library_ids(pool: &sqlx::SqlitePool, files: &[PathBuf]) -> Option<Vec<i64>> {
    let mut ids = Vec::with_capacity(files.len());
    for file in files {
        let id: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM track WHERE file_path = ? AND is_available = 1 LIMIT 1",
        )
        .bind(file.to_string_lossy().as_ref())
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
        ids.push(id?);
    }
    Some(ids)
}

/// Play `paths` — files, folders, or both — from the first file they hold.
/// Library tracks play as such when all of them are; otherwise they play
/// as a queue of files.
pub async fn play_paths(app: &AppHandle, paths: Vec<PathBuf>) -> AppResult<()> {
    // Claimed before the walk and the lookups below (#622): a folder can
    // take a while, and a track the user picks meanwhile must win.
    let intent = app.state::<Arc<AudioEngine>>().next_load_intent();
    play_paths_with_intent(app, paths, intent).await
}

/// [`play_paths`] under an intent the caller claimed earlier: the files a
/// launch hands over are waited for before they play
/// ([`crate::launch_args::play_when_ready`]), and a track the user picks
/// during that wait is the newer choice.
pub async fn play_paths_with_intent(
    app: &AppHandle,
    paths: Vec<PathBuf>,
    intent: LoadIntent,
) -> AppResult<()> {
    let files = tokio::task::spawn_blocking(move || expand(&paths))
        .await
        .map_err(|e| AppError::Other(format!("listing the files to play: {e}")))?;
    if files.is_empty() {
        return Err(AppError::Other("no audio file to play".into()));
    }

    let state = app.state::<AppState>();
    if let Ok(pool) = state.require_profile_pool().await {
        if let Some(ids) = library_ids(&pool, &files).await {
            // Under this open's own intent: it claims the dispatch itself,
            // and gives up if the user picked something since.
            return crate::commands::player::play_tracks_with_intent(
                app.clone(),
                app.state::<AppState>(),
                app.state::<Arc<AudioEngine>>(),
                "manual".into(),
                None,
                ids,
                0,
                None,
                intent,
            )
            .await;
        }
    }

    let entries = files
        .into_iter()
        .map(|path| ExternalEntry { path })
        .collect();
    play_entries(app, entries, 0, 0, intent).await
}

/// The queue a handoff names, without what is no longer there to play, and
/// where its current entry lands in what is left: the entry itself when it
/// is still there, otherwise the next one that is. `None` when nothing is
/// left. Blocking.
fn handed_over_queue(paths: &[PathBuf], index: usize) -> Option<(Vec<PathBuf>, usize)> {
    let mut kept = Vec::with_capacity(paths.len());
    let mut start = None;
    for (i, path) in paths.iter().enumerate() {
        if !(path.is_file() && waveflow_core::scanner::extract::is_scannable_audio(path)) {
            continue;
        }
        if i >= index && start.is_none() {
            start = Some(kept.len());
        }
        kept.push(path.clone());
    }
    let last = kept.len().checked_sub(1)?;
    Some((kept, start.unwrap_or(last)))
}

/// Carry on what another player handed over ([`waveflow_core::handoff`]):
/// its queue, from the track it was on, at the same position, playing or
/// paused as it was. As with [`play_paths`], library tracks play as such
/// when every file is one. The position holds only when the track it
/// belongs to is still there.
pub async fn take_over(
    app: &AppHandle,
    handoff: waveflow_core::handoff::Handoff,
    intent: LoadIntent,
) -> AppResult<()> {
    let playing = handoff.playing;
    let (files, index, position_ms) = tokio::task::spawn_blocking(move || {
        let queue = handed_over_queue(&handoff.paths, handoff.index);
        queue.map(|(files, index)| {
            let same_track = files.get(index) == handoff.paths.get(handoff.index);
            let position_ms = if same_track { handoff.position_ms } else { 0 };
            (files, index, position_ms)
        })
    })
    .await
    .map_err(|e| AppError::Other(format!("listing the handed-over files: {e}")))?
    .ok_or_else(|| AppError::Other("no handed-over file is left to play".into()))?;

    let state = app.state::<AppState>();
    let library = match state.require_profile_pool().await {
        Ok(pool) => library_ids(&pool, &files).await,
        Err(_) => None,
    };
    match library {
        Some(ids) => {
            crate::commands::player::play_tracks_with_intent(
                app.clone(),
                app.state::<AppState>(),
                app.state::<Arc<AudioEngine>>(),
                "manual".into(),
                None,
                ids,
                index,
                None,
                intent,
            )
            .await?;
            // The library load starts at the top; the position follows it
            // down the same channel, before the first sample is decoded.
            if position_ms > 0 {
                settle(app, intent, AudioCmd::Seek(position_ms)).await?;
            }
        }
        None => {
            let entries = files
                .into_iter()
                .map(|path| ExternalEntry { path })
                .collect();
            play_entries(app, entries, index, position_ms, intent).await?;
        }
    }
    // Sent right behind the load, so the decoder takes it before it plays
    // anything: a paused handoff stays silent.
    if !playing {
        settle(app, intent, AudioCmd::Pause).await?;
    }
    Ok(())
}

/// Send `cmd` for the load under `intent`, unless the user has picked
/// something since: the seek or the pause of a handoff belongs to its own
/// track, not to the one that overtook it. Checked and sent under the
/// publish lock (#632), which every load is published under, so no newer
/// load can slip in between and receive it.
async fn settle(app: &AppHandle, intent: LoadIntent, cmd: AudioCmd) -> AppResult<()> {
    let engine = app.state::<Arc<AudioEngine>>();
    let _publish = engine.lock_publish().await;
    if engine.load_intent_superseded(intent) {
        return Ok(());
    }
    engine.send(cmd)
}

/// Install `entries` as the session and start playing at `start_index`,
/// `start_ms` into it. The same guards as
/// [`crate::remote::playback::play_entries`]: nothing is installed for a
/// superseded intent, and an install the user overtook while it started is
/// undone, but only if it is still ours.
async fn play_entries(
    app: &AppHandle,
    entries: Vec<ExternalEntry>,
    start_index: usize,
    start_ms: u64,
    intent: LoadIntent,
) -> AppResult<()> {
    let index = start_index.min(entries.len().saturating_sub(1));
    let engine = app.state::<Arc<AudioEngine>>();
    // Claimed under the publish lock before the first side effect (#632).
    // Reading a file is local and quick, so the lock is held across it,
    // which the remote queue cannot afford over an HTTP round-trip.
    let _publish = engine.lock_publish().await;
    if !engine.claim_dispatch(intent) {
        tracing::debug!("file session superseded before it started; not installing it");
        return Ok(());
    }
    let state = app.state::<AppState>();
    // One detached queue at a time: these files now drive next / previous.
    state.remote_playback.clear();
    let revision = state
        .external_playback
        .set(DetachedQueue { entries, index });
    let played = play_current(app, start_ms, intent).await;
    // A start that did not reach the engine leaves no session behind to
    // drive the next advance — but only the session as it installed it.
    if played.is_err() {
        let _ = engine.send(AudioCmd::Stop);
        state.external_playback.clear_if(revision);
    } else if engine.load_intent_superseded(intent) && state.external_playback.clear_if(revision) {
        tracing::debug!("file session superseded while starting; rolled it back");
    }
    played
}

/// Move the cursor to `index` and play it: the queue panel's click.
pub async fn jump_to(app: &AppHandle, index: usize) -> AppResult<()> {
    let engine = app.state::<Arc<AudioEngine>>();
    let intent = engine.next_load_intent();
    // The cursor moves only for a jump that still has its turn (#632).
    let _publish = engine.lock_publish().await;
    if !engine.claim_dispatch(intent) {
        return Ok(());
    }
    let state = app.state::<AppState>();
    match state.external_playback.seek_to(index) {
        // As in `advance`: a jump that cannot play leaves no session
        // pointing at a file nothing is playing.
        Some(_) => play_current(app, 0, intent).await.inspect_err(|_| {
            let _ = engine.send(AudioCmd::Stop);
            state.external_playback.clear();
        }),
        None => Ok(()),
    }
}

/// Step the cursor and play what it lands on. `Ok(false)` when the queue
/// ran off its end with repeat off: the session is cleared and the engine
/// stopped. `intent` is the caller's, claimed where the step started (a key
/// press, the end of a track), as for the remote queue (#622).
pub async fn advance(app: &AppHandle, direction: Direction, intent: LoadIntent) -> AppResult<bool> {
    let state = app.state::<AppState>();
    let repeat = match state.require_profile_pool().await {
        Ok(pool) => crate::queue::read_repeat_mode(&pool).await,
        Err(_) => crate::queue::RepeatMode::Off,
    };
    let engine = app.state::<Arc<AudioEngine>>();
    // A step overtaken by a newer pick moves nothing (#632): it would
    // otherwise walk the cursor away from the file it never played.
    let _publish = engine.lock_publish().await;
    if !engine.claim_dispatch(intent) {
        return Ok(false);
    }
    match state.external_playback.step(direction, repeat) {
        Some(_) => {
            if let Err(err) = play_current(app, 0, intent).await {
                let _ = engine.send(AudioCmd::Stop);
                state.external_playback.clear();
                return Err(err);
            }
            Ok(true)
        }
        None => {
            let _ = engine.send(AudioCmd::Stop);
            Ok(false)
        }
    }
}

/// Read the file under the cursor and hand it to the engine, to play from
/// `start_ms`.
async fn play_current(app: &AppHandle, start_ms: u64, intent: LoadIntent) -> AppResult<()> {
    let state = app.state::<AppState>();
    let Some(entry) = state.external_playback.current() else {
        return Ok(());
    };
    let artwork_dir = state.paths.metadata_artwork_dir.clone();
    let path = entry.path.clone();
    let facts = tokio::task::spawn_blocking(move || read_facts(&path, &artwork_dir))
        .await
        .map_err(|e| AppError::Other(format!("reading {}: {e}", entry.path.display())))?;
    let engine = app.state::<Arc<AudioEngine>>();
    engine.send(AudioCmd::LoadRemoteFileAndPlay {
        intent,
        origin: DetachedOrigin::File,
        path: entry.path,
        start_ms,
        track_id: crate::commands::player::next_radio_track_id(),
        duration_ms: facts.duration_ms,
        title: Some(facts.title),
        artist: facts.artist,
        artwork_url: facts.cover.map(|p| p.display().to_string()),
        fallback_url: None,
        // The user's own file. Never ours to delete.
        discard_on_failure: false,
        replay_gain: facts.replay_gain,
    })
}

#[cfg(test)]
mod tests;

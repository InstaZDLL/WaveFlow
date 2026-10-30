//! Reading one file into an `ExtractedFile`: its tags, its DSD header,
//! its genres and extra tags, or the stand-in for an untagged file.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, UNIX_EPOCH};

use lofty::file::TaggedFileExt;
use lofty::prelude::{Accessor, AudioFile};
use lofty::tag::ItemKey;

use waveflow_core::scanner::{
    extract_album_artist, extract_compilation_flag, extract_cover, extract_folder_cover,
    extract_musical_key, extract_rating, extract_replay_gain, file_type_label, hash_file,
    split_genres, ExtractedFile, ReplayGainTags,
};

use crate::error::AppResult;

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

    // `is_none`, not "is empty": a tag holding an empty string is a
    // different fault, and `trim(title) = ''` already finds that one.
    let title_from_filename = meta.title.is_none();
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
        title_from_filename,
        abs_path: path.to_string_lossy().to_string(),
        size,
        modified_ms,
        hash,
        title,
        artist: meta.artist,
        album: meta.album,
        // The Album Artist is read (`TPE2` in a DSF, or in a DFF's ID3
        // chunk), because the tag editor writes it there and a scan that
        // ignored it regrouped the track under its first artist. The
        // compilation flag still is not: the reader does not surface it,
        // and the album grouping falls back as it always has.
        album_artist: meta.album_artist,
        is_compilation: false,
        // The id3 crate hands TCON back whole: an ID3v2.4 multi-value
        // frame keeps its NUL separators, which `split_genres` splits.
        genres: split_genres(meta.genre.as_deref()),
        year: meta.year,
        track_number: meta.track_number,
        disc_number: meta.disc_number,
        // The DSD reader does not surface the totals; the completeness
        // check judges these albums on their numbers alone.
        track_total: None,
        disc_total: None,
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
        // The DSF ID3v2 blob can carry ReplayGain frames, but our DSD
        // reader surfaces only the fields it models, not arbitrary tag
        // items. A DSD track falls back to the
        // analysis pass like it did before.
        replay_gain: ReplayGainTags::default(),
        // Same limitation again: the DSD reader surfaces the handful of
        // fields it models and nothing else, so there is no remainder
        // to offer as a custom column (#588) -- bar the composer, which
        // the tag editor writes and so has to come back under the key
        // every other container gives it. Always `Some` and never
        // `None`: this is "read, and these are all", which is true --
        // `None` would mean "could not read", and would stop the
        // scanner from ever clearing a stale row.
        extra_tags: Some(
            meta.composer
                .as_deref()
                .and_then(waveflow_core::scanner::extra_tags::stored_value)
                .map(|composer| {
                    vec![(
                        waveflow_core::scanner::extra_tags::COMPOSER_KEY.to_string(),
                        composer.to_string(),
                    )]
                })
                .unwrap_or_default(),
        ),
    })
}

/// Replace a track's custom tags with what the file now carries (#588).
///
/// Delete-then-insert rather than an upsert: a tag the user *removed*
/// from the file has to disappear from the column too, and an upsert
/// leaves it behind forever. The pair runs inside the scan's own
/// transaction, so a track never has half its old tags and half its new
/// ones.
/// Make `genre_ids` the genres of a track: one `track_genre` row each, the
/// old rows gone. `OR IGNORE` because two spellings `split_genres` kept
/// apart can still share one canonical genre.
pub(super) async fn link_genres(
    tx: &mut sqlx::SqliteConnection,
    track_id: i64,
    genre_ids: &[i64],
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM track_genre WHERE track_id = ?")
        .bind(track_id)
        .execute(&mut *tx)
        .await?;
    for genre_id in genre_ids {
        sqlx::query("INSERT OR IGNORE INTO track_genre (track_id, genre_id) VALUES (?, ?)")
            .bind(track_id)
            .bind(genre_id)
            .execute(&mut *tx)
            .await?;
    }
    // Genres the file carries are the file's, even one equal to what the
    // enrichment engine had filled: its provenance goes, so an undo of
    // the album cannot delete them (`enrichment::store::
    // release_track_fields`, the editor's side of the same rule). A file
    // with none leaves it, and the engine's next pass fills the gap again.
    if !genre_ids.is_empty() {
        sqlx::query("DELETE FROM musicbrainz_filled_genre WHERE track_id = ?")
            .bind(track_id)
            .execute(&mut *tx)
            .await?;
    }
    Ok(())
}

pub(super) async fn write_extra_tags(
    tx: &mut sqlx::SqliteConnection,
    track_id: i64,
    tags: Option<&Vec<(String, String)>>,
) -> AppResult<()> {
    // A read that failed leaves what is stored alone. Replacing it with
    // nothing would lose a track's tags to a file that happened to be
    // locked for the length of one parse.
    let Some(tags) = tags else { return Ok(()) };
    sqlx::query("DELETE FROM track_tag WHERE track_id = ?")
        .bind(track_id)
        .execute(&mut *tx)
        .await?;
    for (key, value) in tags {
        sqlx::query("INSERT INTO track_tag (track_id, key, value) VALUES (?, ?, ?)")
            .bind(track_id)
            .bind(key)
            .bind(value)
            .execute(&mut *tx)
            .await?;
    }
    Ok(())
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
pub(super) struct ScanTimings {
    pub(super) hash_us: AtomicU64,
    pub(super) tag_us: AtomicU64,
    /// The second, concrete tag parse that reads the custom frames
    /// (#588). Timed apart from `tag_us` on purpose: it is the one cost
    /// that column feature adds to every scan, and burying it inside
    /// the total would make it impossible to answer whether it is worth
    /// paying. Summed across the parallel extraction tasks, like
    /// `tag_us`.
    pub(super) extra_tag_us: AtomicU64,
    /// Wall time spent on the SERIAL per-track DB work in the consumer
    /// loop (the `SELECT existing` probe + every `upsert_*` + the row
    /// INSERT/UPDATE + the periodic `TX_BATCH` commit). Unlike
    /// `hash_us` / `tag_us` — summed across the parallel extraction
    /// tasks — this accrues on the single consumer task, so the total
    /// is already wall-clock: it tells us directly how much of
    /// `extract_db_ms` is the single-writer DB path vs the parallel
    /// extraction feeding it. Diagnostics only.
    pub(super) db_us: AtomicU64,
    /// Subset of `db_us` spent inside `maybe_link_artist_images` — the
    /// per-track sidecar-artist-image walk (`fs::read_dir` of up to 3
    /// ancestor dirs + a `canonical_name` per directory entry). On a
    /// library with no local artist images this re-walks the same
    /// folders for the same artists on every track, so it's the prime
    /// suspect for the chunk of `db_ms_total` that the upsert
    /// memoisation did NOT move. Diagnostics only — split out to
    /// confirm before optimising (the SQL-lookup memo was measured to
    /// barely dent `db_ms_total`, so guessing isn't enough).
    pub(super) link_us: AtomicU64,
}

/// A playable file no tag reader accepts (#873), as the library lists it:
/// titled after its file name, described by the decoder, covered by a
/// folder image when there is one. Every tag field is empty, so a later
/// rescan after the tags are repaired fills them in as for any file.
fn untagged_file(
    path: &Path,
    artwork_dir: &Path,
    size: i64,
    modified_ms: i64,
    hash: String,
    stream: waveflow_core::scanner::extract::StreamInfo,
) -> ExtractedFile {
    ExtractedFile {
        title_from_filename: true,
        abs_path: path.to_string_lossy().to_string(),
        size,
        modified_ms,
        hash,
        title: path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string(),
        artist: None,
        album: None,
        album_artist: None,
        is_compilation: false,
        genres: Vec::new(),
        year: None,
        track_number: None,
        disc_number: None,
        track_total: None,
        disc_total: None,
        duration_ms: stream.duration_ms,
        bitrate: None,
        sample_rate: stream.sample_rate,
        channels: stream.channels,
        bit_depth: stream.bit_depth,
        codec: stream.codec,
        musical_key: None,
        cover_art: extract_folder_cover(path, artwork_dir),
        rating: None,
        replay_gain: ReplayGainTags::default(),
        extra_tags: Default::default(),
    }
}

pub(super) fn extract_file(
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
    // By content, not extension, and leniently on a malformed tag
    // (`read_tagged_file`). Should lofty still refuse the file, it is
    // kept when the player can play it: listed by its file name, with
    // what the decoder reports, rather than dropped from the library
    // over tags nobody can read (#873).
    let tagged = match waveflow_core::scanner::extract::read_tagged_file(path) {
        Ok(tagged) => tagged,
        Err(err) => {
            let stream = waveflow_core::scanner::extract::probe_playable_stream(path)
                .ok_or_else(|| format!("lofty: {err}"))?;
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "tags unreadable; listing the file from its audio stream"
            );
            timings
                .tag_us
                .fetch_add(t_tag.elapsed().as_micros() as u64, Ordering::Relaxed);
            return Ok(untagged_file(
                path,
                artwork_dir,
                size,
                modified_ms,
                hash,
                stream,
            ));
        }
    };
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
        genres,
        year,
        track_number,
        disc_number,
        track_total,
        disc_total,
        cover_art,
        rating,
        musical_key,
        replay_gain,
    ) = match tag {
        Some(tag) => (
            tag.title().map(|s| s.into_owned()),
            tag.artist().map(|s| s.into_owned()),
            tag.album().map(|s| s.into_owned()),
            extract_album_artist(tag),
            extract_compilation_flag(tag),
            // Every genre item, not `tag.genre()`, which is the first one
            // only: lofty already turns a multi-value TCON, repeated
            // Vorbis GENRE comments and several MP4 `©gen` into one item
            // each (#729).
            split_genres(tag.get_strings(ItemKey::Genre)),
            tag.date().map(|d| d.year as i64),
            tag.track().map(|n| n as i64),
            tag.disk().map(|n| n as i64),
            tag.track_total().map(|n| n as i64),
            tag.disk_total().map(|n| n as i64),
            extract_cover(tag, artwork_dir),
            extract_rating(tag),
            extract_musical_key(tag),
            extract_replay_gain(tag),
        ),
        None => (
            None,
            None,
            None,
            None,
            false,
            Vec::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            ReplayGainTags::default(),
        ),
    };

    // The custom frames, from a second parse of the concrete container
    // (#588). They are not in what lofty just handed back: the generic
    // `Tag` drops everything it cannot map onto an `ItemKey`, and the
    // remainder it keeps for some formats is `pub(crate)`. Best-effort —
    // a file whose custom frames cannot be read is still a good track.
    let t_extra = Instant::now();
    let extra_tags = waveflow_core::scanner::extra_tags::read_extra_tags(path);
    timings
        .extra_tag_us
        .fetch_add(t_extra.elapsed().as_micros() as u64, Ordering::Relaxed);

    // Folder cover fallback: scan the track's parent directory for a
    // sidecar cover.jpg / folder.png / front.webp / ... when the tag had
    // no embedded picture. Common for CD rips and lossless libraries
    // where the artwork lives next to the audio files.
    let cover_art = cover_art.or_else(|| extract_folder_cover(path, artwork_dir));

    // Fall back to the file stem when the tag has no title — better than
    // displaying an empty string in the library grid. Remembered, so
    // the inventory can still tell the two apart.
    let title_from_filename = title.is_none();
    let title = title.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string()
    });

    Ok(ExtractedFile {
        title_from_filename,
        abs_path: path.to_string_lossy().to_string(),
        size,
        modified_ms,
        hash,
        title,
        artist,
        album,
        album_artist,
        is_compilation,
        genres,
        year,
        track_number,
        disc_number,
        track_total,
        disc_total,
        duration_ms,
        bitrate,
        sample_rate,
        channels,
        bit_depth,
        codec,
        musical_key,
        cover_art,
        rating,
        replay_gain,
        extra_tags,
    })
}

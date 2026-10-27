//! The `lyrics` cache rows: writing a hit or a miss, reading one back,
//! and the sidecar exported after a fetch when the user asked for one.

use std::path::Path;
use std::time::Duration;

use waveflow_syncedlyrics::LyricsResult as ExternalLyricsResult;

use crate::error::AppResult;

use super::*;

pub(super) async fn cache_external_lyrics(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    file_hash: &str,
    result: ExternalLyricsResult,
) -> AppResult<LyricsPayload> {
    let format = external_format_to_app(result.format);
    let source = LyricsSource::Api;
    let provider = result.provider.as_str();
    upsert_lyrics(
        pool,
        file_hash,
        &result.content,
        &format,
        &source,
        Some(provider),
    )
    .await?;
    Ok(LyricsPayload {
        track_id,
        content: result.content,
        format,
        source,
        provider: Some(provider.to_string()),
        tag_write_skipped: None,
        associated: Vec::new(),
    })
}

/// Write the primary lyrics row. The one spelling of that statement.
///
/// Takes the connection rather than the pool so both callers can run it
/// inside their own transaction — `upsert_lyrics` pairs it with clearing
/// the bundle, `cache_lyrics_bundle` with inserting a new one, and
/// neither can be allowed to commit half of that.
///
/// `language` is what the SOURCE said, not a user preference: only the
/// v2 plugin world reports one today, and every other tier passes `None`
/// exactly as this row has always been written.
///
/// `retry_after` is written on every call, `NULL` included, so a row that
/// replaces a partial miss never inherits its expiry — lyrics found after
/// one must not be thrown away a week later.
#[allow(clippy::too_many_arguments)]
pub(super) async fn write_primary_lyrics(
    conn: &mut sqlx::SqliteConnection,
    file_hash: &str,
    content: &str,
    format: &LyricsFormat,
    source: &LyricsSource,
    provider: Option<&str>,
    language: Option<&str>,
    fetched_at: i64,
    retry_after: Option<i64>,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO app.lyrics
            (file_hash, content, format, source, provider, language, fetched_at, retry_after)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(file_hash) DO UPDATE SET
            content = excluded.content,
            format = excluded.format,
            source = excluded.source,
            provider = excluded.provider,
            language = excluded.language,
            fetched_at = excluded.fetched_at,
            retry_after = excluded.retry_after",
    )
    .bind(file_hash)
    .bind(content)
    .bind(format_to_db(format))
    .bind(source_to_db(source))
    .bind(provider)
    .bind(language)
    .bind(fetched_at)
    .bind(retry_after)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Read a cached bundle's associated documents.
async fn read_associated(
    pool: &sqlx::SqlitePool,
    track_id: i64,
) -> AppResult<Vec<AssociatedLyrics>> {
    let rows: Vec<(String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT a.kind, a.language, a.content, a.format
           FROM track t
           JOIN app.lyrics_associated a ON a.file_hash = t.file_hash
          WHERE t.id = ?
          ORDER BY a.kind, a.language",
    )
    .bind(track_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(kind, language, content, format)| AssociatedLyrics {
            kind,
            language,
            content,
            format: parse_format(&format),
        })
        .collect())
}

/// How long a partial miss (#720) is served before the track is looked up
/// again. Long enough that a provider down for good costs one lookup per
/// track per week instead of one per panel open; short enough that a
/// provider which was only briefly down gets its chance.
const PARTIAL_MISS_RETRY: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// When a miss written now should be looked up again: never for a
/// complete one, after [`PARTIAL_MISS_RETRY`] for a partial one.
pub(super) fn miss_retry_after(partial: bool) -> Option<i64> {
    partial.then(|| now_ms() + PARTIAL_MISS_RETRY.as_millis() as i64)
}

/// Cache an empty "miss" row so the panel doesn't re-hit the network on
/// every open, and return it as a payload. The user can force a
/// re-search via "Refetch" in the lyrics panel (clears the row, re-runs
/// the waterfall). `provider = None` — attribution to a specific provider
/// would be misleading when every provider returned nothing.
pub(super) async fn cache_lyrics_miss(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    meta: &TrackMeta,
    partial: bool,
) -> AppResult<LyricsPayload> {
    let empty = String::new();
    upsert_lyrics_until(
        pool,
        &meta.file_hash,
        &empty,
        &LyricsFormat::Plain,
        &LyricsSource::Api,
        None,
        miss_retry_after(partial),
    )
    .await?;
    Ok(LyricsPayload {
        track_id,
        content: empty,
        format: LyricsFormat::Plain,
        source: LyricsSource::Api,
        provider: None,
        tag_write_skipped: None,
        associated: Vec::new(),
    })
}

/// Insert (or replace) the lyrics row, keyed by file content hash so the
/// cache is shared across profiles that contain the same audio file.
///
/// `provider` carries the sub-source identifier when `source` is
/// `LyricsSource::Api` (e.g. `"lrclib"`, `"genius"`) and `None` for
/// embedded / sidecar / manual writes where the broad `source` is
/// the only meaningful attribution. The DB column allows NULL so
/// pre-1.5.1 rows + non-API tiers store cleanly.
pub(super) async fn upsert_lyrics(
    pool: &sqlx::SqlitePool,
    file_hash: &str,
    content: &str,
    format: &LyricsFormat,
    source: &LyricsSource,
    provider: Option<&str>,
) -> AppResult<()> {
    upsert_lyrics_until(pool, file_hash, content, format, source, provider, None).await
}

/// [`upsert_lyrics`] for a row that is only good until `retry_after`
/// (epoch ms) — a partial miss (#720). `None` is a row kept for good.
pub(super) async fn upsert_lyrics_until(
    pool: &sqlx::SqlitePool,
    file_hash: &str,
    content: &str,
    format: &LyricsFormat,
    source: &LyricsSource,
    provider: Option<&str>,
    retry_after: Option<i64>,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    write_primary_lyrics(
        &mut tx,
        file_hash,
        content,
        format,
        source,
        provider,
        None,
        now_ms(),
        retry_after,
    )
    .await?;
    // Replacing the primary document ends the bundle it belonged to.
    // Translations and pronunciations are cached as part of ONE fetch
    // result (issue #585), so leaving them behind would pair new lyrics
    // with the previous provider's companions — and nothing downstream
    // could tell they were never published together. Every tier that
    // writes a primary comes through here, so the invariant holds
    // without each of them having to remember it.
    sqlx::query("DELETE FROM app.lyrics_associated WHERE file_hash = ?")
        .bind(file_hash)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    // After the commit, never inside the transaction: a sidecar that
    // could not be written must not cost the user the lyrics themselves
    // (#695).
    export_fetched_sidecar(pool, file_hash, content, format, source).await;
    Ok(())
}

/// Also write freshly fetched lyrics next to the audio file, when the
/// user asked for sidecars (issue #695).
///
/// Until now the destination chosen in onboarding / Settings governed
/// lyrics the user **typed**; anything fetched from LRCLIB, Musixmatch or
/// a plugin stayed in `app.lyrics` alone, so a library carefully set to
/// "sidecar file" still ended up with its fetched lyrics locked inside
/// WaveFlow's database. Now they travel with the music: a reinstall, a
/// copy to another machine, another player reading the same folder.
///
/// Four rules, each of them a refusal:
///
/// - **Only what came off the network.** `Embedded` and `LrcFile` are
///   already on disk, and `Manual` goes through [`save_lyrics`], which
///   asks where that one edit should land and honours all three answers.
/// - **`Tag` is deliberately not honoured here.** Writing a tag rewrites
///   the audio file, and the file a fetch is about is usually the one
///   playing — which is why the tag editor pauses playback and re-hashes
///   ([`invariants.md`](../../../../../docs/architecture/invariants.md#file-write-safety-on-windows)).
///   A fetch that happened on its own must not do that behind the user's
///   back, so the cache row carries it and the editor stays the way
///   lyrics reach a tag.
/// - **Never overwrite.** A sidecar already there is the user's, or an
///   earlier export's. In the usual order this cannot even arise — the
///   local tier would have found it and the source would be `LrcFile` —
///   but under `lyrics.prefer_lrclib` the network runs first.
/// - **A failure is not an error.** A read-only library folder, a NAS
///   that went away, a filename the filesystem refuses: the database row
///   is the source of truth and lyrics must still show. Logged, swallowed.
pub(super) async fn export_fetched_sidecar(
    pool: &sqlx::SqlitePool,
    file_hash: &str,
    content: &str,
    format: &LyricsFormat,
    source: &LyricsSource,
) {
    if !matches!(source, LyricsSource::Api) {
        return;
    }
    // An empty row is the instrumental marker, not lyrics to carry.
    if content.trim().is_empty() {
        return;
    }
    match read_default_destination(pool).await {
        Ok(LyricsDestination::Sidecar) => {}
        Ok(_) => return,
        Err(err) => {
            tracing::debug!(?err, "lyrics sidecar export: destination unreadable");
            return;
        }
    }

    // The cache is keyed by content hash, which can name several files —
    // the same track twice in one library, or a copy the user keeps
    // elsewhere. Each copy is a file the user may open in another player,
    // so each gets its own sidecar; in practice there is one row.
    let paths: Vec<String> =
        match sqlx::query_scalar("SELECT file_path FROM track WHERE file_hash = ?")
            .bind(file_hash)
            .fetch_all(pool)
            .await
        {
            Ok(rows) => rows,
            Err(err) => {
                tracing::debug!(?err, "lyrics sidecar export: track lookup failed");
                return;
            }
        };

    for path in paths {
        let content = content.to_string();
        let format = *format;
        let _ = tokio::task::spawn_blocking(move || {
            let audio = Path::new(&path);
            let Some(sidecar) = sidecar_path(audio, &format) else {
                return;
            };
            // Ask the reader the waterfall itself uses, rather than probing
            // one exact path: it matches the stem case-insensitively and
            // looks in a sibling `Lyrics/` folder too. Probing
            // `Song.lrc` alone would write a second file beside a
            // `song.LRC` the user made, on any case-sensitive filesystem --
            // and a file that supersedes theirs is the thing this refuses
            // to do, whatever it is called.
            if read_sidecar_lyrics(audio).is_some() {
                return;
            }
            // `create_new` rather than `write`, for the two files that
            // reader says nothing about: one that is empty or all
            // whitespace (it reports those as misses, and truncating one
            // would still be writing over something we did not create),
            // and one that appears between the check above and this line.
            // The kernel decides, so there is no window left to lose.
            let created = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&sidecar);
            let mut file = match created {
                Ok(file) => file,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return,
                Err(err) => {
                    tracing::debug!(
                        %err,
                        path = %sidecar.display(),
                        "lyrics sidecar export failed"
                    );
                    return;
                }
            };
            if let Err(err) = std::io::Write::write_all(&mut file, content.as_bytes()) {
                // The file is ours — `create_new` made it — and what is
                // in it is cut short. Left there, the local tier would
                // read it as the track's lyrics and this export, which
                // never overwrites, would never put it right.
                drop(file);
                let _ = std::fs::remove_file(&sidecar);
                tracing::debug!(
                    %err,
                    path = %sidecar.display(),
                    "lyrics sidecar export failed"
                );
            } else {
                tracing::debug!(path = %sidecar.display(), "exported fetched lyrics");
            }
        })
        .await;
    }
}

/// Read the cached lyrics row, if any. The frontend identifies tracks by
/// numeric `track_id` so we look up the file hash first, then key into the
/// shared `app.lyrics` cache.
pub(super) async fn read_cached(
    pool: &sqlx::SqlitePool,
    track_id: i64,
) -> AppResult<Option<LyricsPayload>> {
    let row: Option<(String, String, String, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT l.content, l.format, l.source, l.provider, l.retry_after
           FROM track t
           JOIN app.lyrics l ON l.file_hash = t.file_hash
          WHERE t.id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await?;

    // A partial miss past its date reads as "not cached", so the caller
    // runs the waterfall again (#720). Kept rather than deleted: a later
    // online lookup or the prefetch overwrites it once it has a verdict.
    let row = row.and_then(
        |(content, format, source, provider, retry_after)| match retry_after {
            Some(due) if due <= now_ms() => None,
            _ => Some((content, format, source, provider)),
        },
    );

    // A cached row that is a distribution service's credit rather than
    // lyrics is dropped and re-resolved, rather than served forever.
    //
    // The predicate keys on `source = 'embedded'`, which is also what
    // the description tier stores under — `lyrics.source` is
    // CHECK-constrained to four values, and giving the description its
    // own would mean rebuilding a shared cache table for a
    // classification nicety. The practical consequence is one absurd
    // edge: a genuine `USLT` tag whose opening really is a YouTube
    // credit gets dropped and re-read from the file on every panel
    // open. The listener sees the same text either way; it costs one
    // file read.
    //
    // Only here, and deliberately: `run_prefetch` picks its work with
    // `WHERE l.file_hash IS NULL`, so it still skips a track holding
    // one of these rows. Teaching it otherwise would mean spelling the
    // recogniser a second time in SQL, and two spellings of one
    // predicate drift. The healing happens when the panel is opened,
    // which is the moment the wrong text would have been read.
    //
    // Fixing `read_description_lyrics` alone would only have helped
    // tracks nobody had opened yet: the waterfall never refetches once
    // a row exists, so the person who reported this would have seen
    // nothing change. The row is *provably* not lyrics — it is the
    // auto-generated YouTube credit — which is what makes deleting it
    // safe rather than presumptuous, and it costs one string scan on a
    // path that already does a join.
    if let Some((content, _, source, _)) = row.as_ref() {
        if source == "embedded" && is_service_blurb(content) {
            let _ = sqlx::query(
                "DELETE FROM app.lyrics
                  WHERE file_hash = (SELECT file_hash FROM track WHERE id = ?)",
            )
            .bind(track_id)
            .execute(pool)
            .await;
            tracing::info!(
                track_id,
                "dropped a cached service credit that had been stored as lyrics"
            );
            return Ok(None);
        }
    }

    let Some((content, fmt, src, provider)) = row else {
        return Ok(None);
    };

    // The companions are read separately rather than joined in above: a
    // bundle has zero rows in the common case, and a LEFT JOIN would
    // multiply the primary row by however many documents came with it
    // only to collapse it again here.
    //
    // Nothing needs to filter out stale ones. They cascade with the
    // primary — including on the service-credit delete just above — and
    // every write of a primary clears them, so a row that exists here
    // was cached by the same fetch as the content beside it.
    let associated = read_associated(pool, track_id).await?;

    Ok(Some(LyricsPayload {
        track_id,
        content,
        format: parse_format(&fmt),
        source: parse_source(&src),
        provider,
        tag_write_skipped: None,
        associated,
    }))
}

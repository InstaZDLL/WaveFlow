//! Mood-based radio — Spotify-style "moment of the day" queues.
//!
//! Five presets. Each one is a **tempo gate plus a shape**: the gate
//! decides what may be considered, and [`waveflow_core::mood`] ranks
//! everything inside it by how well it actually fits — distance from
//! the mood's tempo centre, loudness, and any genre word the mood
//! names. The forty tracks that play are the best of the pool, not the
//! first forty drawn out of it (#616).
//!
//! # Why mostly tempo gates
//!
//! A gate answers "is this the wrong kind of track"; a score answers
//! "how right is it". Tempo is the signal where falling outside the
//! range most clearly means the wrong mood — a 160 BPM track is not
//! Sleep at any loudness. Loudness and genre are ranked instead, which
//! is what lets a thin library still return forty tracks, closest fits
//! first, rather than an error.
//!
//! Sleep is the exception, with a loudness gate as well
//! (`lufs_ceiling`): the tempo estimator rarely reads under 60 BPM, so
//! a slow window also holds fast tracks read as 60-something, and only
//! loudness tells them apart. Without it Sleep held nine tenths of a
//! library.
//!
//! # The user's last word
//!
//! No gate tells every slow ballad from a rock track the estimator read
//! at 60 BPM, so a track can be put into a mood or taken out of one by
//! hand (`mood_override`). A track taken out is never drawn for that
//! mood, in either mode; a track put in plays ahead of the ranking in
//! track mode and counts on the home tile whatever its analysis says.
//! Album mode plays whole records, so it honours removals only.
//!
//! Returns the ordered track IDs plus whether they are whole records
//! ([`MoodRadioSession`]). The frontend hands both to
//! `player_play_tracks` with `source_type = "radio"` so
//! play_event rows still get tagged for stats — the existing CHECK
//! constraint on `queue_item.source_type` doesn't allow a `'mood'`
//! variant and adding one would mean a migration just for analytics
//! granularity, which isn't worth the churn yet.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use waveflow_core::mood::{MoodCandidate, MoodProfile};

use crate::{
    commands::track::{track_from_row, Track},
    error::{AppError, AppResult},
    state::AppState,
};
use waveflow_core::repository::{sqlite::SqliteTrackRepository, track::TrackRepository};

/// Target queue size. Same logic as the seed-based radio: long enough
/// to forget about it, short enough that the tail stays on-vibe.
const TARGET_LEN: usize = 40;

/// Hard cap on tracks per primary artist. Without this the mood radio
/// would collapse to "your top artist on repeat" once a heavy listener
/// has 100+ play_events on a single act.
const PER_ARTIST_CAP: usize = 4;

/// Pool size before ranking. Drawn at random, which is what keeps two
/// runs of the same mood from being the same queue; the ranking then
/// decides which forty of it play. Larger = more variety in the
/// candidate set, but also slower SQL — 400 hits the sweet spot for
/// libraries up to ~50k tracks.
const POOL_SIZE: i64 = 400;

use waveflow_core::album_playback::{ALBUM_FIT, ALBUM_MIN_ANALYSED};

/// Album mode: albums pulled before budgeting. Smaller than
/// [`POOL_SIZE`] because each row is a whole record rather than one
/// track.
const ALBUM_POOL_SIZE: i64 = 60;

/// Album mode: records per primary artist. Lower than
/// [`PER_ARTIST_CAP`] because a record is already several tracks of
/// the same artist in a row.
const ALBUM_PER_ARTIST_CAP: usize = 2;

/// The tempo gate, in SQL: does any octave reading of `ta.bpm` fall
/// inside `?1 .. ?2`? Doubled only toward a ceiling and halved only
/// toward a floor, as [`waveflow_core::mood::effective_bpm`] reads it:
/// halving into a window with no floor (Sleep) admitted every track up
/// to twice its ceiling.
///
/// Written once because three queries ask it — the track pool, the
/// album pool and the count behind the home tile — and a copy that
/// drifted would make a mood report a number it cannot deliver, or
/// hide one it can. A macro rather than a `const`, so the fragment is
/// pasted by `concat!` and every query stays a `&'static str` literal:
/// building the string at runtime would mean handing sqlx SQL it
/// cannot verify.
macro_rules! bpm_any_octave {
    () => {
        concat!(
            "(   ",
            bpm_plain_in_window!(),
            " OR (?2 IS NOT NULL AND (?1 IS NULL OR ta.bpm * 2.0 >= ?1) AND ta.bpm * 2.0 <= ?2)",
            " OR (?1 IS NOT NULL AND ta.bpm / 2.0 >= ?1 AND (?2 IS NULL OR ta.bpm / 2.0 <= ?2)))"
        )
    };
}

/// The tempo gate for the **measured** reading alone.
///
/// Half of [`bpm_any_octave!`], and also what orders the draw: a
/// corrected reading is a guess, and a guess must not take a place in
/// the pool from a track that really is this tempo: drawing at random
/// across both would fill a mood with rescued readings whenever the
/// genuinely matching tracks are rare.
macro_rules! bpm_plain_in_window {
    () => {
        "((?1 IS NULL OR ta.bpm >= ?1) AND (?2 IS NULL OR ta.bpm <= ?2))"
    };
}

/// The loudness gate, in SQL, against the placeholder `$p` bound to
/// the profile's `lufs_ceiling`: as
/// [`waveflow_core::mood::passes_loudness_gate`], a track measured
/// louder is out and an unmeasured one passes. Shared by the same
/// three queries as the tempo gate, for the same reason.
macro_rules! lufs_under_ceiling {
    ($p:literal) => {
        concat!(
            "(",
            $p,
            " IS NULL OR ta.loudness_lufs IS NULL OR ta.loudness_lufs <= ",
            $p,
            ")"
        )
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mood {
    Focus,
    Chill,
    Workout,
    Party,
    Sleep,
}

impl Mood {
    const ALL: [Mood; 5] = [
        Mood::Focus,
        Mood::Chill,
        Mood::Workout,
        Mood::Party,
        Mood::Sleep,
    ];

    /// The spelling `mood_override.mood` stores, which its CHECK lists.
    fn as_str(self) -> &'static str {
        match self {
            Mood::Focus => "focus",
            Mood::Chill => "chill",
            Mood::Workout => "workout",
            Mood::Party => "party",
            Mood::Sleep => "sleep",
        }
    }

    fn parse(s: &str) -> Option<Mood> {
        Mood::ALL.into_iter().find(|m| m.as_str() == s)
    }

    /// The five shapes.
    ///
    /// The tempo windows are narrower than they were: Chill's used to
    /// sit **entirely inside** Focus's, and Party shared fifteen beats
    /// with Workout, so two different moods could return the same kind
    /// of list. They still overlap where the moods genuinely do —
    /// Focus and Chill are both slow and calm, and no tempo separates
    /// them — and there the centres, the loudness and the genre words
    /// do the separating.
    fn profile(&self) -> MoodProfile {
        match self {
            // Calm, mid-tempo, quiet. The −14 LUFS preference is
            // roughly Spotify's normalisation target; louder than that
            // tends to pull attention away from whatever you are
            // focusing on.
            Mood::Focus => MoodProfile {
                bpm_min: Some(72.0),
                bpm_max: Some(108.0),
                bpm_centre: 88.0,
                lufs_max: Some(-14.0),
                lufs_min: None,
                lufs_ceiling: None,
                genre_words: &[
                    "ambient",
                    "classical",
                    "instrumental",
                    "piano",
                    "soundtrack",
                    "score",
                    "post-rock",
                ],
            },
            // Lounge / chill territory: slower than Focus on average,
            // and separated from it mostly by genre — "calm enough to
            // work to" and "calm enough to sit in" are the same tempo.
            Mood::Chill => MoodProfile {
                bpm_min: Some(65.0),
                bpm_max: Some(95.0),
                bpm_centre: 78.0,
                lufs_max: Some(-10.0),
                lufs_min: None,
                lufs_ceiling: None,
                genre_words: &[
                    "chill",
                    "lounge",
                    "downtempo",
                    "soul",
                    "jazz",
                    "bossa",
                    "r&b",
                    "trip hop",
                    "folk",
                ],
            },
            // Workout: keeps the cadence above ~128 (running,
            // lifting), and wants the loud end of the library rather
            // than merely tolerating it. Upper bound at 180 to avoid
            // the drum'n'bass and speedcore most users wouldn't want
            // for a treadmill session.
            Mood::Workout => MoodProfile {
                bpm_min: Some(128.0),
                bpm_max: Some(180.0),
                bpm_centre: 150.0,
                lufs_max: None,
                lufs_min: Some(-12.0),
                lufs_ceiling: None,
                genre_words: &[
                    "electronic",
                    "dance",
                    "techno",
                    "rock",
                    "metal",
                    "punk",
                    "hip hop",
                    "rap",
                    "drum",
                ],
            },
            // Dance-pop tempo band, narrowed off Workout's: they now
            // share four beats instead of fifteen.
            Mood::Party => MoodProfile {
                bpm_min: Some(110.0),
                bpm_max: Some(132.0),
                bpm_centre: 122.0,
                lufs_max: None,
                lufs_min: Some(-12.0),
                lufs_ceiling: None,
                genre_words: &[
                    "dance",
                    "pop",
                    "house",
                    "disco",
                    "funk",
                    "reggaeton",
                    "electronic",
                    "latin",
                ],
            },
            // Sleep: very slow, very quiet. The ceiling sits where the
            // loudness ranking reaches zero (−18 plus its six-decibel
            // run): past it a track is the wrong loudness, not a weaker
            // fit. Mastered pop sits around −8, so it is out; quiet
            // acoustic and ambient records stay in.
            Mood::Sleep => MoodProfile {
                bpm_min: None,
                bpm_max: Some(68.0),
                bpm_centre: 52.0,
                lufs_max: Some(-18.0),
                lufs_min: None,
                lufs_ceiling: Some(-12.0),
                genre_words: &[
                    "ambient",
                    "classical",
                    "piano",
                    "meditation",
                    "drone",
                    "new age",
                    "sleep",
                ],
            },
        }
    }
}

/// What a mood produced, and what shape it is in.
///
/// The track list alone could not say whether the backend answered in
/// whole records or fell back to individual tracks — and the caller
/// needs to know, because that is what decides whether the session gets
/// album gain (#647). A bare `Vec<i64>` made the two look identical.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoodRadioSession {
    pub track_ids: Vec<i64>,
    /// Whole records in their own order. **What was produced, not what
    /// was asked for**: album mode is a preference and the fallback
    /// below is deliberate, so a mood that found no qualifying record
    /// answers `false` even with the setting on.
    pub album_ordered: bool,
}

#[tauri::command]
pub async fn start_mood_radio(
    state: tauri::State<'_, AppState>,
    mood: Mood,
) -> AppResult<MoodRadioSession> {
    let pool = state.require_profile_pool().await?;
    let profile = mood.profile();

    if waveflow_core::album_playback::album_mode_enabled(&pool).await {
        // Album mode is a preference, not a contract: a library whose
        // records are mostly unanalysed can satisfy the mood track by
        // track while no whole record qualifies. Falling through then
        // plays music instead of returning an error — and if there is
        // genuinely nothing, the track path says so in the words that
        // actually apply ("no tracks match this mood") rather than
        // blaming the albums.
        let mut by_album = mood_radio_by_album(&pool, mood).await?;
        // A record keeps its order but loses the tracks taken out of
        // this mood: the record was chosen, those tracks were not. The
        // query already left them out of its counts; this drops them
        // from the queue, which reads every track of the record.
        let excluded: std::collections::HashSet<i64> =
            overridden_tracks(&pool, mood, OverrideKind::Exclude)
                .await?
                .into_iter()
                .collect();
        by_album.retain(|id| !excluded.contains(id));
        if !by_album.is_empty() {
            return Ok(MoodRadioSession {
                track_ids: by_album,
                album_ordered: true,
            });
        }
        tracing::info!("no record fits this mood; falling back to individual tracks");
    }

    // The tracks put into this mood by hand come first, in a fresh
    // order each run, and the ranking fills what they leave. They skip
    // the per-artist cap: the user chose each one.
    let mut track_ids = overridden_tracks(&pool, mood, OverrideKind::Include).await?;
    track_ids.truncate(TARGET_LEN);
    if track_ids.len() == TARGET_LEN {
        // The hand-picked tracks fill the radio: nothing to rank.
        return Ok(MoodRadioSession {
            track_ids,
            album_ordered: false,
        });
    }
    let rows: Vec<MoodCandidate> = candidate_pool(&pool, mood, POOL_SIZE)
        .await?
        .into_iter()
        .filter(|c| !track_ids.contains(&c.track_id))
        .collect();
    if rows.is_empty() && track_ids.is_empty() {
        return Err(AppError::Other(
            "no tracks match this mood — run BPM analysis on your library first".into(),
        ));
    }
    let room = TARGET_LEN - track_ids.len();
    track_ids.extend(waveflow_core::mood::rank_and_cap(
        &profile,
        rows,
        room,
        PER_ARTIST_CAP,
    ));

    Ok(MoodRadioSession {
        track_ids,
        // The fallback path, and every run with the setting off: a list
        // of individual tracks, whatever they happen to be.
        album_ordered: false,
    })
}

/// The tracks a mood may consider, drawn at random.
///
/// `bpm IS NOT NULL` is mandatory — a tempo-based mood cannot be
/// honoured without it. The gate accepts **any octave reading**
/// (`bpm`, `bpm × 2`, `bpm ÷ 2`): estimators land an octave out often
/// enough that a 170 BPM track is recorded as 85, and without this the
/// correction in [`waveflow_core::mood`] would never see the tracks it
/// exists to rescue. The corrected reading is discounted when scoring,
/// so those tracks sit below the honest ones rather than beside them.
///
/// Loudness and genre rank rather than filter, except for a mood with a
/// loudness gate (Sleep). See the module header.
async fn candidate_pool(
    pool: &SqlitePool,
    mood: Mood,
    limit: i64,
) -> AppResult<Vec<MoodCandidate>> {
    let profile = mood.profile();
    #[derive(sqlx::FromRow)]
    struct Row {
        track_id: i64,
        primary_artist: Option<i64>,
        bpm: f64,
        loudness_lufs: Option<f64>,
        genres: Option<String>,
    }

    let rows: Vec<Row> = sqlx::query_as::<_, Row>(concat!(
        "SELECT t.id                AS track_id,
                t.primary_artist    AS primary_artist,
                ta.bpm              AS bpm,
                ta.loudness_lufs    AS loudness_lufs,
                (SELECT GROUP_CONCAT(g.name, ' ')
                   FROM track_genre tg JOIN genre g ON g.id = tg.genre_id
                  WHERE tg.track_id = t.id) AS genres
           FROM track t
           JOIN track_analysis ta ON ta.track_id = t.id
          WHERE t.is_available = 1
            AND ta.bpm IS NOT NULL
            AND ta.bpm > 0
            AND NOT EXISTS (SELECT 1 FROM mood_override mo
                             WHERE mo.track_id = t.id AND mo.mood = ?5
                               AND mo.kind = 'exclude')
            AND ",
        lufs_under_ceiling!("?4"),
        " AND ",
        bpm_any_octave!(),
        // Measured matches first, still shuffled among themselves; the
        // corrected ones only top up what is left. The album path does
        // not need this: it is ranked on the record's average tempo,
        // which the scorer discounts the same way, and its gate already
        // asks that most of the record fit — a record of corrected
        // readings rarely clears that.
        " ORDER BY CASE WHEN ",
        bpm_plain_in_window!(),
        " THEN 0 ELSE 1 END, RANDOM()
          LIMIT ?3"
    ))
    .bind(profile.bpm_min)
    .bind(profile.bpm_max)
    .bind(limit)
    .bind(profile.lufs_ceiling)
    .bind(mood.as_str())
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| MoodCandidate {
            track_id: r.track_id,
            primary_artist: r.primary_artist,
            bpm: r.bpm,
            loudness_lufs: r.loudness_lufs,
            genres: r.genres,
        })
        .collect())
}

/// The same radio, built out of whole records (#618).
///
/// Selection is per album rather than per track: a record qualifies
/// when most of what we have measured of it sits inside the mood's
/// tempo window, and the records that qualify are then **ranked by the
/// fit of their average track** rather than drawn at random — the same
/// change as the track path, applied to the unit album mode plays in.
/// Each record then plays in the order it was pressed. The per-artist
/// cap becomes a cap on records, for the same reason as on tracks:
/// without it a heavy listener's mood radio is one artist's
/// discography.
async fn mood_radio_by_album(pool: &SqlitePool, mood: Mood) -> AppResult<Vec<i64>> {
    let profile = &mood.profile();
    #[derive(sqlx::FromRow)]
    struct Row {
        album_id: i64,
        /// Every playable track on the record, analysed or not — this
        /// is the budgeting unit, and it has to match what actually
        /// gets queued.
        track_count: i64,
        /// `track.primary_artist` is nullable (`ON DELETE SET NULL`),
        /// so an album whose tracks have all lost theirs aggregates to
        /// NULL. Decoding that into an `i64` fails the whole query,
        /// which would take the radio down with it.
        primary_artist: Option<i64>,
        /// The record judged as one track: the mood ranks it by the
        /// average of what was measured on it.
        avg_bpm: Option<f64>,
        avg_lufs: Option<f64>,
    }

    let rows: Vec<Row> = sqlx::query_as::<_, Row>(concat!(
        "SELECT t.album_id             AS album_id,
                COUNT(*)               AS track_count,
                MIN(t.primary_artist)  AS primary_artist,
                AVG(ta.bpm)            AS avg_bpm,
                AVG(ta.loudness_lufs)  AS avg_lufs
           FROM track t
           LEFT JOIN track_analysis ta ON ta.track_id = t.id
          WHERE t.is_available = 1
            AND t.album_id IS NOT NULL
            -- Out of the fit and of the budget: a track taken out of
            -- this mood is not queued, so it must not be counted.
            AND NOT EXISTS (SELECT 1 FROM mood_override mo
                             WHERE mo.track_id = t.id AND mo.mood = ?7
                               AND mo.kind = 'exclude')
          GROUP BY t.album_id
         HAVING SUM(CASE WHEN ta.bpm IS NOT NULL THEN 1 ELSE 0 END) >= ?4
            AND SUM(CASE WHEN ta.bpm IS NOT NULL AND ta.bpm > 0 AND ",
        lufs_under_ceiling!("?6"),
        " AND ",
        bpm_any_octave!(),
        "          THEN 1 ELSE 0 END) * 1.0
                / SUM(CASE WHEN ta.bpm IS NOT NULL THEN 1 ELSE 0 END) >= ?3
            -- A record plays whole, so one track measured over the
            -- loudness gate rules it out: the fit above only asks that
            -- most of it be right, and would queue the loud track too.
            AND SUM(CASE WHEN NOT ",
        lufs_under_ceiling!("?6"),
        " THEN 1 ELSE 0 END) = 0
          ORDER BY RANDOM()
          LIMIT ?5"
    ))
    .bind(profile.bpm_min)
    .bind(profile.bpm_max)
    .bind(ALBUM_FIT)
    .bind(ALBUM_MIN_ANALYSED)
    .bind(ALBUM_POOL_SIZE)
    .bind(profile.lufs_ceiling)
    .bind(mood.as_str())
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        // Empty rather than an error: the caller decides what an empty
        // album selection means, and it means "try tracks".
        return Ok(vec![]);
    }

    // Rank the records the way the track path ranks tracks, by reusing
    // the same scorer over the record's averages. `album_id` stands in
    // for the track id, so the tie-break stays total. The cap is a cap
    // on records here, and `rank_and_cap` applies it to the ranking,
    // so what it drops is that artist's worst-fitting record.
    let ranked = waveflow_core::mood::rank_and_cap(
        profile,
        rows.iter()
            .map(|r| MoodCandidate {
                track_id: r.album_id,
                primary_artist: r.primary_artist,
                // A record only qualified because it has analysed
                // tracks, so this is `None` in no reachable case; zero
                // then scores as the worst possible tempo rather than
                // taking the radio down.
                bpm: r.avg_bpm.unwrap_or(0.0),
                loudness_lufs: r.avg_lufs,
                genres: None,
            })
            .collect(),
        rows.len(),
        ALBUM_PER_ARTIST_CAP,
    );

    let track_counts: std::collections::HashMap<i64, i64> =
        rows.iter().map(|r| (r.album_id, r.track_count)).collect();
    let candidates: Vec<waveflow_core::album_playback::AlbumCandidate> = ranked
        .into_iter()
        .filter_map(|album_id| {
            track_counts.get(&album_id).map(|&track_count| {
                waveflow_core::album_playback::AlbumCandidate {
                    album_id,
                    track_count,
                }
            })
        })
        .collect();

    let chosen = waveflow_core::album_playback::fit_albums_to_budget(&candidates, TARGET_LEN);
    Ok(waveflow_core::album_playback::tracks_in_album_order(pool, &chosen).await?)
}

/// Returns how many library tracks would qualify for each mood, so the
/// frontend can disable buttons that would yield empty radios (typical
/// for libraries where BPM analysis hasn't been run yet, or where the
/// user has no slow tracks at all).
///
/// The counts answer the gates only — tempo, and loudness where a mood
/// has one — because they are what can rule a track out; see the
/// module header. They come
/// with how much of the library has been analysed at all, so the UI
/// can say *why* a mood is thin instead of leaving the user to guess.
#[tauri::command]
pub async fn mood_radio_counts(state: tauri::State<'_, AppState>) -> AppResult<MoodCounts> {
    let pool = state.require_profile_pool().await?;
    let (analysed_tracks, total_tracks) = analysis_coverage(&pool).await?;
    Ok(MoodCounts {
        focus: count_for_mood(&pool, Mood::Focus).await?,
        chill: count_for_mood(&pool, Mood::Chill).await?,
        workout: count_for_mood(&pool, Mood::Workout).await?,
        party: count_for_mood(&pool, Mood::Party).await?,
        sleep: count_for_mood(&pool, Mood::Sleep).await?,
        analysed_tracks,
        total_tracks,
    })
}

#[derive(Debug, serde::Serialize)]
pub struct MoodCounts {
    pub focus: i64,
    pub chill: i64,
    pub workout: i64,
    pub party: i64,
    pub sleep: i64,
    /// Tracks carrying a tempo measurement — the population every mood
    /// draws from.
    pub analysed_tracks: i64,
    /// Playable tracks in the library, analysed or not.
    pub total_tracks: i64,
}

/// How much of the library has a tempo, and how big the library is.
async fn analysis_coverage(pool: &SqlitePool) -> AppResult<(i64, i64)> {
    // `SUM` over an empty set is NULL, and an empty library is a real
    // state (a fresh profile) — decoding that into `i64` would fail
    // the command on exactly the install that has nothing to show.
    let row: (Option<i64>, i64) = sqlx::query_as(
        r#"
        SELECT SUM(CASE WHEN ta.bpm IS NOT NULL THEN 1 ELSE 0 END),
               COUNT(*)
          FROM track t
          LEFT JOIN track_analysis ta ON ta.track_id = t.id
         WHERE t.is_available = 1
        "#,
    )
    .fetch_one(pool)
    .await?;
    Ok((row.0.unwrap_or(0), row.1))
}

/// What a mood can draw from: the tracks its gates admit, less those
/// taken out of it by hand, plus those put into it by hand whatever
/// their analysis says.
async fn count_for_mood(pool: &SqlitePool, mood: Mood) -> AppResult<i64> {
    let profile = mood.profile();
    let n: i64 = sqlx::query_scalar(concat!(
        "SELECT COUNT(*)
           FROM track t
           LEFT JOIN track_analysis ta ON ta.track_id = t.id
           LEFT JOIN mood_override mo ON mo.track_id = t.id AND mo.mood = ?4
          WHERE t.is_available = 1
            AND (mo.kind = 'include'
                 OR (mo.kind IS NULL
                     AND ta.bpm IS NOT NULL
                     AND ta.bpm > 0
                     AND ",
        lufs_under_ceiling!("?3"),
        " AND ",
        bpm_any_octave!(),
        "))"
    ))
    .bind(profile.bpm_min)
    .bind(profile.bpm_max)
    .bind(profile.lufs_ceiling)
    .bind(mood.as_str())
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// A track put into a mood by hand, or taken out of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OverrideKind {
    Include,
    Exclude,
}

impl OverrideKind {
    fn as_str(self) -> &'static str {
        match self {
            OverrideKind::Include => "include",
            OverrideKind::Exclude => "exclude",
        }
    }

    fn parse(s: &str) -> Option<OverrideKind> {
        match s {
            "include" => Some(OverrideKind::Include),
            "exclude" => Some(OverrideKind::Exclude),
            _ => None,
        }
    }
}

/// The playable tracks of one kind of override for a mood, in a fresh
/// random order.
async fn overridden_tracks(
    pool: &SqlitePool,
    mood: Mood,
    kind: OverrideKind,
) -> AppResult<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT mo.track_id
           FROM mood_override mo
           JOIN track t ON t.id = mo.track_id
          WHERE mo.mood = ? AND mo.kind = ? AND t.is_available = 1
          ORDER BY RANDOM()",
    )
    .bind(mood.as_str())
    .bind(kind.as_str())
    .fetch_all(pool)
    .await?)
}

#[derive(Debug, Serialize)]
pub struct TrackMoodOverride {
    pub mood: Mood,
    pub kind: OverrideKind,
}

/// The moods a track was put into or taken out of by hand, for the
/// track menu to show where it stands.
#[tauri::command]
pub async fn track_mood_overrides(
    state: tauri::State<'_, AppState>,
    track_id: i64,
) -> AppResult<Vec<TrackMoodOverride>> {
    let pool = state.require_profile_pool().await?;
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT mood, kind FROM mood_override WHERE track_id = ?")
            .bind(track_id)
            .fetch_all(&*pool)
            .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(mood, kind)| {
            Some(TrackMoodOverride {
                mood: Mood::parse(&mood)?,
                kind: OverrideKind::parse(&kind)?,
            })
        })
        .collect())
}

/// Put `track_ids` into `mood`, take them out of it, or — with `kind`
/// absent — hand them back to the gates.
#[tauri::command]
pub async fn set_mood_override(
    state: tauri::State<'_, AppState>,
    mood: Mood,
    track_ids: Vec<i64>,
    kind: Option<OverrideKind>,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let now = chrono::Utc::now().timestamp_millis();
    let mut tx = pool.begin().await?;
    for track_id in track_ids {
        match kind {
            Some(kind) => {
                sqlx::query(
                    "INSERT INTO mood_override (mood, track_id, kind, added_at)
                     VALUES (?, ?, ?, ?)
                     ON CONFLICT(mood, track_id)
                     DO UPDATE SET kind = excluded.kind, added_at = excluded.added_at",
                )
                .bind(mood.as_str())
                .bind(track_id)
                .bind(kind.as_str())
                .bind(now)
                .execute(&mut *tx)
                .await?;
            }
            None => {
                sqlx::query("DELETE FROM mood_override WHERE mood = ? AND track_id = ?")
                    .bind(mood.as_str())
                    .bind(track_id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct MoodOverrideTrack {
    pub kind: OverrideKind,
    pub track: Track,
}

/// Every track put into or taken out of `mood` by hand, newest first,
/// for the mood's page.
#[tauri::command]
pub async fn list_mood_overrides(
    state: tauri::State<'_, AppState>,
    mood: Mood,
) -> AppResult<Vec<MoodOverrideTrack>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT track_id, kind FROM mood_override
          WHERE mood = ? ORDER BY added_at DESC, track_id",
    )
    .bind(mood.as_str())
    .fetch_all(&*pool)
    .await?;
    let repo = SqliteTrackRepository::new((*pool).clone());
    let mut out = Vec::with_capacity(rows.len());
    for (track_id, kind) in rows {
        let (Some(kind), Some(row)) = (OverrideKind::parse(&kind), repo.get(track_id).await?)
        else {
            continue;
        };
        out.push(MoodOverrideTrack {
            kind,
            track: track_from_row(row, &artwork_dir),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// The repo's own profile migrations, with `foreign_keys` on.
    ///
    /// A hand-written fixture would let these queries pass against a
    /// schema the app never has — the same reason the inventory tests
    /// run the real migrations.
    async fn pool() -> SqlitePool {
        let options = SqliteConnectOptions::from_str(":memory:")
            .unwrap()
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("../../migrations/profile")
            .run(&pool)
            .await
            .unwrap();
        pool
    }

    /// Every query in this module runs, against the real schema, for
    /// every mood.
    ///
    /// The tempo gate is pasted into three statements by a macro and
    /// none of them is a compile-time-checked query: a missing
    /// parenthesis or a column that does not exist would compile
    /// perfectly and fail the first time a user pressed a mood tile.
    /// An empty library is enough to prove the SQL parses and that the
    /// placeholders line up with the binds.
    #[tokio::test]
    async fn every_mood_query_parses_and_binds() {
        let pool = pool().await;
        for mood in [
            Mood::Focus,
            Mood::Chill,
            Mood::Workout,
            Mood::Party,
            Mood::Sleep,
        ] {
            assert!(candidate_pool(&pool, mood, 10).await.unwrap().is_empty());
            assert!(mood_radio_by_album(&pool, mood).await.unwrap().is_empty());
            assert_eq!(count_for_mood(&pool, mood).await.unwrap(), 0);
            for kind in [OverrideKind::Include, OverrideKind::Exclude] {
                assert!(overridden_tracks(&pool, mood, kind)
                    .await
                    .unwrap()
                    .is_empty());
            }
        }
        assert_eq!(analysis_coverage(&pool).await.unwrap(), (0, 0));
    }

    /// An octave-out tempo reaches the pool, which is what the gate in
    /// SQL exists for — the scorer cannot rescue a row the query never
    /// returned.
    #[tokio::test]
    async fn the_pool_accepts_a_doubled_tempo() {
        let pool = pool().await;
        sqlx::query(
            "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'l', 0, 0);
             INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                file_modified, title, duration_ms, added_at)
                  VALUES (1, 1, '/l/a.flac', 'h1', 1, 0, 'Fast', 200000, 0),
                         (2, 1, '/l/b.flac', 'h2', 1, 0, 'Slow', 200000, 0),
                         (3, 1, '/l/c.flac', 'h3', 1, 0, 'Plain', 200000, 0);
             -- 30 BPM, not 40: doubled, 40 lands on 80, which is
             -- inside Focus. Every reading of 30 (30, 60, 15) is out.
             INSERT INTO track_analysis (track_id, bpm, analyzed_at)
                  VALUES (1, 170.0, 0), (2, 30.0, 0), (3, 90.0, 0);",
        )
        .execute(&pool)
        .await
        .unwrap();

        let ids: Vec<i64> = candidate_pool(&pool, Mood::Focus, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.track_id)
            .collect();
        assert!(
            ids.contains(&1),
            "170 BPM must reach a mood built on 88, as a halved reading"
        );
        assert!(!ids.contains(&2), "30 BPM is out under every reading");
        assert_eq!(count_for_mood(&pool, Mood::Focus).await.unwrap(), 2);

        // A measured match comes before a corrected one, whatever the
        // shuffle does inside each group: a guess must not take a place
        // in the pool from a track that really is this tempo.
        assert_eq!(
            ids.first(),
            Some(&3),
            "90 BPM is measured inside the window; 170 is a rescue"
        );
    }

    /// Sleep keeps a quiet slow track and nothing else: not a fast one
    /// read at half speed, not a loud one the estimator read as slow.
    /// Before, the halved reading alone let in every track up to 136.
    #[tokio::test]
    async fn sleep_keeps_only_quiet_slow_tracks() {
        let pool = pool().await;
        sqlx::query(
            "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'l', 0, 0);
             INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                file_modified, title, duration_ms, added_at)
                  VALUES (1, 1, '/l/a.flac', 'h1', 1, 0, 'Lullaby', 200000, 0),
                         (2, 1, '/l/b.flac', 'h2', 1, 0, 'Dance', 200000, 0),
                         (3, 1, '/l/c.flac', 'h3', 1, 0, 'Loud', 200000, 0),
                         (4, 1, '/l/d.flac', 'h4', 1, 0, 'Unmeasured', 200000, 0);
             INSERT INTO track_analysis (track_id, bpm, loudness_lufs, analyzed_at)
                  VALUES (1, 55.0, -20.0, 0), (2, 120.0, -16.0, 0),
                         (3, 62.0, -7.0, 0), (4, 60.0, NULL, 0);",
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut ids: Vec<i64> = candidate_pool(&pool, Mood::Sleep, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.track_id)
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 4]);
        assert_eq!(count_for_mood(&pool, Mood::Sleep).await.unwrap(), 2);

        // Taken out by hand: out of the pool and the count. Put in by
        // hand: counted, though its tempo is not Sleep's at all.
        sqlx::query(
            "INSERT INTO mood_override (mood, track_id, kind, added_at)
                  VALUES ('sleep', 4, 'exclude', 0), ('sleep', 2, 'include', 0),
                         ('focus', 1, 'exclude', 0);",
        )
        .execute(&pool)
        .await
        .unwrap();
        let ids: Vec<i64> = candidate_pool(&pool, Mood::Sleep, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.track_id)
            .collect();
        assert_eq!(ids, vec![1], "another mood's removal does not apply");
        assert_eq!(count_for_mood(&pool, Mood::Sleep).await.unwrap(), 2);
        assert_eq!(
            overridden_tracks(&pool, Mood::Sleep, OverrideKind::Include)
                .await
                .unwrap(),
            vec![2]
        );
    }

    /// In album mode a record plays whole, so a single track measured
    /// over Sleep's loudness gate rules the record out — the tempo fit
    /// only asks that most of it be right, and would queue that track.
    #[tokio::test]
    async fn sleep_refuses_a_record_with_one_loud_track() {
        let pool = pool().await;
        sqlx::query(
            "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'l', 0, 0);
             INSERT INTO album (id, title) VALUES (1, 'Quiet'), (2, 'Mostly quiet');
             INSERT INTO track (id, library_id, album_id, track_number, file_path,
                                file_hash, file_size, file_modified, title,
                                duration_ms, added_at)
                  VALUES (1, 1, 1, 1, '/q/1.flac', 'q1', 1, 0, 'a', 200000, 0),
                         (2, 1, 1, 2, '/q/2.flac', 'q2', 1, 0, 'b', 200000, 0),
                         (3, 1, 1, 3, '/q/3.flac', 'q3', 1, 0, 'c', 200000, 0),
                         (4, 1, 2, 1, '/m/1.flac', 'm1', 1, 0, 'd', 200000, 0),
                         (5, 1, 2, 2, '/m/2.flac', 'm2', 1, 0, 'e', 200000, 0),
                         (6, 1, 2, 3, '/m/3.flac', 'm3', 1, 0, 'f', 200000, 0);
             INSERT INTO track_analysis (track_id, bpm, loudness_lufs, analyzed_at)
                  VALUES (1, 55.0, -20.0, 0), (2, 55.0, -20.0, 0), (3, 55.0, NULL, 0),
                         (4, 55.0, -20.0, 0), (5, 55.0, -20.0, 0), (6, 55.0, -7.0, 0);",
        )
        .execute(&pool)
        .await
        .unwrap();

        let ids = mood_radio_by_album(&pool, Mood::Sleep).await.unwrap();
        assert_eq!(ids, vec![1, 2, 3], "an unmeasured track does not count");

        // Taking the loud track out of Sleep by hand lets its record in:
        // what is left of it is all quiet.
        sqlx::query(
            "INSERT INTO mood_override (mood, track_id, kind, added_at)
                  VALUES ('sleep', 6, 'exclude', 0);",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut ids = mood_radio_by_album(&pool, Mood::Sleep).await.unwrap();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec![1, 2, 3, 4, 5, 6],
            "the queue reads the whole record"
        );
    }
}

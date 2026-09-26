use crate::audio::replay_gain::TrackGain;
use waveflow_core::analysis::ANALYSIS_VERSION;

/// Look up what is known about a track's loudness, from both sources
/// at once: the ReplayGain the file carries in its own tags (read by
/// the scanner into `track`) and what our analysis pass measured
/// (`track_analysis`). The tag wins where both exist — see
/// [`TrackGain::prefer_tag`].
///
/// An empty result means "leave the signal untouched", which is the
/// safe default and what a failed lookup returns too.
///
/// Called at every `LoadAndPlay` / `SetNextTrack` dispatch site so the
/// decoder thread never has to reach into SQLite from the audio path.
pub(crate) async fn fetch_replay_gain(pool: &sqlx::SqlitePool, track_id: i64) -> TrackGain {
    #[allow(clippy::type_complexity)]
    let row = sqlx::query_as::<
        _,
        (
            Option<f64>,
            Option<f64>,
            Option<f64>,
            Option<f64>,
            Option<f64>,
            Option<f64>,
            Option<i64>,
        ),
    >(
        // The album pair has no analysis counterpart to join against:
        // our pass measures one track at a time, so album gain is
        // whatever a tagger wrote into the file, or nothing.
        "SELECT t.rg_track_gain_db, t.rg_track_peak,
                t.rg_album_gain_db, t.rg_album_peak,
                a.replay_gain_db, a.peak, a.analysis_version
           FROM track t
           LEFT JOIN track_analysis a ON a.track_id = t.id
          WHERE t.id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    let Some((
        tag_gain,
        tag_peak,
        tag_album_gain,
        tag_album_peak,
        analysis_gain,
        analysis_peak,
        analysis_version,
    )) = row
    else {
        return TrackGain::default();
    };
    TrackGain::prefer_tag(
        TrackGain {
            gain_db: tag_gain,
            peak: tag_peak,
            peak_unverified: false,
            album_gain_db: tag_album_gain,
            album_peak: tag_album_peak,
        },
        TrackGain {
            gain_db: analysis_gain,
            peak: analysis_peak,
            // Anything that is not the generation we know, we cannot
            // vouch for. NULL is the case this was built for — a row
            // predating the column, whose peak came from the mono
            // downmix used before #545 and reads lower than the real
            // one. But the test is deliberately not `is_none()`: a row
            // left by a *different* version is equally unaccounted for,
            // whether older (a bump we made) or newer (a profile
            // restored from a build ahead of this one). Clipping
            // prevention then reads the peak as a lower bound rather
            // than a measurement.
            //
            // `LEFT JOIN` also yields NULL when there is no analysis row
            // at all — harmless, because `peak` is then NULL too and the
            // flag never gets read.
            peak_unverified: analysis_version != Some(ANALYSIS_VERSION),
            // Never set on this side: see the comment on the query.
            album_gain_db: None,
            album_peak: None,
        },
    )
}

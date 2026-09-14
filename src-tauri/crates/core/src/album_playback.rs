//! Building a listening session out of whole records rather than
//! scattered tracks (#618).
//!
//! Mood Radio and the Daily Mix both pick tracks one at a time, which
//! is the right shape for a playlist and the wrong one for someone
//! who listens to albums: a record arrives as three of its tracks,
//! out of order, between two unrelated artists.
//!
//! Album mode inverts that. The *selection* stays each generator's own
//! business — a mood is a BPM window, a Daily Mix bucket is a cluster
//! of artists — and what lives here is the part they share and that is
//! easy to get wrong: turning a list of chosen albums into a track
//! list that never cuts one of them in half.

#[cfg(feature = "sqlite")]
use sqlx::SqlitePool;

#[cfg(feature = "sqlite")]
use crate::error::CoreResult;

/// How much of a record has to sit inside a tempo window for the
/// record to count as fitting it.
///
/// A fraction rather than a median. A median says nothing about
/// spread: a record that is half ambient and half thrash has a median
/// in the middle and would be offered for a mood neither of its halves
/// belongs to. Asking that most of it actually fits rejects that
/// record from every mood, which is the right answer.
///
/// Lives here rather than in each caller because Mood Radio and the
/// Daily Mix have to agree on it — two copies of a threshold is two
/// answers to one question, and they drift.
pub const ALBUM_FIT: f64 = 0.6;

/// Fewest analysed tracks before a record's fit is worth believing.
/// Below this one outlier decides the whole thing, and a single-track
/// "album" would qualify for whatever window it happens to match.
pub const ALBUM_MIN_ANALYSED: i64 = 3;

/// An album that qualified, with enough of it measured to be worth
/// trusting.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct AlbumCandidate {
    pub album_id: i64,
    /// Every playable track on the record, analysed or not.
    ///
    /// Deliberately not "the analysed ones": this is the budgeting
    /// unit, so it has to match what
    /// [`tracks_in_album_order`] will actually queue. The fit fraction
    /// is measured over the analysed subset, which is a different
    /// count and a different question.
    pub track_count: i64,
}

/// Fit whole albums into a track budget.
///
/// The rule the budget exists for: **an album is taken entire or not
/// at all**. Truncating the candidate list at N tracks, which is what
/// a track-based generator does, would end a session halfway through
/// a record — the one thing album mode is supposed to prevent.
///
/// A single album longer than the whole budget is still taken, because
/// the alternative is an empty session on a library of long records.
/// Everything after it is then measured against the budget as usual,
/// so one double LP does not licence a second.
///
/// Pure, so the budgeting is testable without a database — the
/// selection queries above it are not, and this is where the
/// off-by-an-album lives.
pub fn fit_albums_to_budget(candidates: &[AlbumCandidate], budget: usize) -> Vec<i64> {
    let mut chosen = Vec::new();
    let mut total = 0usize;
    for candidate in candidates {
        let len = candidate.track_count.max(0) as usize;
        if len == 0 {
            continue;
        }
        // The first album always goes in: a budget smaller than the
        // shortest record should still produce a session.
        if !chosen.is_empty() && total + len > budget {
            continue;
        }
        chosen.push(candidate.album_id);
        total += len;
        if total >= budget {
            break;
        }
    }
    chosen
}

/// Whether Mood Radio and the Daily Mix should build their sessions
/// out of whole records.
///
/// One setting for both, because it is one preference: someone who
/// listens to albums wants albums from anything that builds them a
/// session. Splitting it in two would ask the same question twice and
/// let the answers disagree.
///
/// Defaults to off, and an unreadable row reads as off — this changes
/// what a generator produces, so it should only ever happen because
/// somebody asked for it.
#[cfg(feature = "sqlite")]
pub async fn album_mode_enabled(pool: &SqlitePool) -> bool {
    matches!(
        sqlx::query_scalar::<_, String>(
            "SELECT value FROM profile_setting WHERE key = 'playback.generator_album_mode'",
        )
        .fetch_optional(pool)
        .await,
        Ok(Some(ref value)) if value == "true"
    )
}

/// Expand chosen albums into their tracks, each record in the order it
/// was pressed.
///
/// The albums keep the order they were given — that is the caller's
/// shuffle, already applied — and only the tracks inside each one are
/// sorted. An untagged track sorts after every numbered one, for the
/// same reason it does in the queue: a bonus track with no number
/// belongs at the end of the record, not in front of track 1.
///
/// Only the SQLite build reaches a queue: the Postgres side of this
/// crate is the server's, which has no playback. The budgeting above
/// stays ungated so it is compiled and tested either way.
#[cfg(feature = "sqlite")]
pub async fn tracks_in_album_order(pool: &SqlitePool, album_ids: &[i64]) -> CoreResult<Vec<i64>> {
    if album_ids.is_empty() {
        return Ok(vec![]);
    }
    let placeholders = std::iter::repeat("?")
        .take(album_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    // `COALESCE(..., 1 << 30)` is the SQL spelling of "nulls last" that
    // works on every SQLite the app ships against; `NULLS LAST` needs
    // 3.30 and the bundled version is not ours to assume.
    let sql = format!(
        r#"
        SELECT t.id AS id, t.album_id AS album_id
          FROM track t
         WHERE t.album_id IN ({placeholders})
           AND t.is_available = 1
         ORDER BY COALESCE(t.disc_number, 1 << 30),
                  COALESCE(t.track_number, 1 << 30),
                  t.id
        "#,
    );
    let mut query = sqlx::query_as::<_, (i64, i64)>(sqlx::AssertSqlSafe(sql));
    for id in album_ids {
        query = query.bind(*id);
    }
    let rows = query.fetch_all(pool).await?;

    // Regroup in the order the caller asked for. One pass, so an album
    // list that came back shuffled is not re-sorted by the database.
    let mut by_album: std::collections::HashMap<i64, Vec<i64>> = std::collections::HashMap::new();
    for (track_id, album_id) in rows {
        by_album.entry(album_id).or_default().push(track_id);
    }
    Ok(album_ids
        .iter()
        .filter_map(|id| by_album.remove(id))
        .flatten()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(album_id: i64, track_count: i64) -> AlbumCandidate {
        AlbumCandidate {
            album_id,
            track_count,
        }
    }

    /// The whole point: the budget is spent in whole records, so a
    /// session never stops halfway through one.
    #[test]
    fn albums_are_taken_whole_or_not_at_all() {
        let candidates = [album(1, 10), album(2, 12), album(3, 9)];
        let chosen = fit_albums_to_budget(&candidates, 25);
        assert_eq!(chosen, vec![1, 2], "22 fits, 31 would not");
    }

    /// An album that does not fit is skipped rather than ending the
    /// selection, so a long record early in the list does not starve
    /// the short ones behind it.
    #[test]
    fn an_album_that_does_not_fit_is_skipped_not_final() {
        let candidates = [album(1, 8), album(2, 40), album(3, 6)];
        assert_eq!(fit_albums_to_budget(&candidates, 20), vec![1, 3]);
    }

    /// A budget smaller than the shortest record still produces a
    /// session — an empty Daily Mix is worse than a long one.
    #[test]
    fn the_first_album_is_taken_however_long_it_is() {
        let candidates = [album(1, 80), album(2, 4)];
        assert_eq!(fit_albums_to_budget(&candidates, 50), vec![1]);
    }

    /// …and that exception is spent once. The over-long first album
    /// has already blown the budget, so nothing follows it.
    #[test]
    fn one_over_long_album_does_not_licence_a_second() {
        let candidates = [album(1, 80), album(2, 80), album(3, 2)];
        assert_eq!(fit_albums_to_budget(&candidates, 50), vec![1]);
    }

    /// An album whose tracks are all unavailable or unanalysed carries
    /// no weight and must not take a slot.
    #[test]
    fn empty_albums_are_ignored() {
        let candidates = [album(1, 0), album(2, -3), album(3, 5)];
        assert_eq!(fit_albums_to_budget(&candidates, 10), vec![3]);
    }

    #[test]
    fn an_empty_candidate_list_yields_an_empty_session() {
        assert!(fit_albums_to_budget(&[], 50).is_empty());
    }
}

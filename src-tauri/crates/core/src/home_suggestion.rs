//! The track Home suggests, and why.
//!
//! Home's suggestion card shows one track with a reason, drawn from three
//! kinds:
//!
//! - **Forgotten**: a track played through several times that has not
//!   been played for a while.
//! - **Vibe**: a track in line with what has been playing lately — by
//!   one of the artists played most in the last two weeks or one similar
//!   to them, close to their energy and tempo, and not played recently.
//! - **Unplayed**: a track never played, by an artist played often when
//!   there is one, else among the latest added.
//!
//! The pick is **stable for a day**: the same day and the same number of
//! "Another suggestion" clicks give the same track, so Home does not
//! change under the user on every visit. The kind rotates from one day to
//! the next, and each click moves to the next kind with a new draw.
//!
//! What lives here is the arithmetic — the rotation, the draw, the per-
//! artist cap, the distance to the recent listening — so it can be tested
//! against values. The queries are the app's (`commands::home_suggestion`).

use serde::{Deserialize, Serialize};

/// Plays that count: played through, or for at least 15 s. The same rule
/// as the recently-played list, so a track skipped after two seconds is
/// not a favourite.
pub const QUALIFIED_PLAY_MS: i64 = 15_000;
/// A forgotten favourite has been played through at least this often…
pub const FORGOTTEN_MIN_PLAYS: i64 = 3;
/// …and not at all for this many days.
pub const FORGOTTEN_IDLE_DAYS: i64 = 60;
/// How far back "what has been playing lately" looks.
pub const VIBE_LOOKBACK_DAYS: i64 = 14;
/// A vibe pick has not been played for this many days: the point is a
/// track that has not just been heard.
pub const VIBE_FRESH_DAYS: i64 = 30;
/// Artists the recent listening is summed up by.
pub const VIBE_TOP_ARTISTS: usize = 5;
/// Tracks the draw is made among, best first.
pub const SHORTLIST_LEN: usize = 20;
/// No artist takes more than this many places on a shortlist, or the
/// most-played artist would be every suggestion.
pub const SHORTLIST_PER_ARTIST: usize = 2;

pub const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SuggestionKind {
    Forgotten,
    Vibe,
    Unplayed,
}

const KINDS: [SuggestionKind; 3] = [
    SuggestionKind::Forgotten,
    SuggestionKind::Vibe,
    SuggestionKind::Unplayed,
];

/// The kinds to try, in order, for `day` (days since the epoch, in the
/// user's time zone) after `attempt` clicks on "Another suggestion".
///
/// The first is the one this draw is about; the others are the fallback
/// when it has nothing (a new library has no forgotten favourite).
pub fn kind_order(day: i64, attempt: u32) -> [SuggestionKind; 3] {
    let start = (day + i64::from(attempt)).rem_euclid(3) as usize;
    [KINDS[start], KINDS[(start + 1) % 3], KINDS[(start + 2) % 3]]
}

/// Which of `len` shortlisted tracks to suggest. The same `day`,
/// `attempt` and `kind` always give the same index.
pub fn pick_index(len: usize, day: i64, attempt: u32, kind: SuggestionKind) -> usize {
    if len == 0 {
        return 0;
    }
    let seed = (day as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(u64::from(attempt).wrapping_mul(0xBF58_476D_1CE4_E5B9))
        .wrapping_add(kind as u64);
    (splitmix64(seed) % len as u64) as usize
}

fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The first `limit` items, best first, keeping at most `per_artist` per
/// artist. An item without an artist is never capped.
pub fn shortlist<T>(
    ranked: impl IntoIterator<Item = T>,
    artist_of: impl Fn(&T) -> Option<i64>,
    per_artist: usize,
    limit: usize,
) -> Vec<T> {
    let mut taken: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    let mut out = Vec::new();
    for item in ranked {
        if out.len() >= limit {
            break;
        }
        if let Some(artist) = artist_of(&item) {
            let count = taken.entry(artist).or_insert(0);
            if *count >= per_artist {
                continue;
            }
            *count += 1;
        }
        out.push(item);
    }
    out
}

/// The average energy and tempo of what has been playing lately, each
/// `None` when no recent play was analysed.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RecentProfile {
    pub energy: Option<f64>,
    pub bpm: Option<f64>,
}

/// How far a track sits from the recent listening; lower is closer.
///
/// Energy (0–1) counts in full. Tempo counts per 100 BPM, and a half or a
/// double tempo is as close as the tempo itself, since the estimator
/// reads some tracks an octave out. A value missing on either side counts
/// as halfway, so an unanalysed track is neither preferred nor ruled out.
pub fn vibe_distance(recent: RecentProfile, energy: Option<f64>, bpm: Option<f64>) -> f64 {
    const UNKNOWN: f64 = 0.5;
    let energy_part = match (recent.energy, energy) {
        (Some(a), Some(b)) => (a - b).abs(),
        _ => UNKNOWN,
    };
    let tempo_part = match (recent.bpm, bpm) {
        (Some(a), Some(b)) if a > 0.0 && b > 0.0 => [b, b * 2.0, b / 2.0]
            .iter()
            .map(|candidate| (a - candidate).abs() / 100.0)
            .fold(f64::INFINITY, f64::min)
            .min(1.0),
        _ => UNKNOWN,
    };
    energy_part + tempo_part
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kind_rotates_by_day_and_by_click() {
        let a = kind_order(20_000, 0);
        let b = kind_order(20_001, 0);
        let c = kind_order(20_000, 1);
        assert_ne!(a[0], b[0]);
        assert_eq!(b[0], c[0]);
        // Every kind is tried, each once.
        for order in [a, b, c] {
            for kind in KINDS {
                assert_eq!(order.iter().filter(|k| **k == kind).count(), 1);
            }
        }
        // Three clicks come back to the day's kind.
        assert_eq!(kind_order(20_000, 3)[0], a[0]);
    }

    #[test]
    fn the_draw_is_stable_and_in_range() {
        for len in 1..30 {
            let first = pick_index(len, 20_000, 2, SuggestionKind::Vibe);
            assert!(first < len);
            assert_eq!(first, pick_index(len, 20_000, 2, SuggestionKind::Vibe));
        }
        assert_eq!(pick_index(0, 20_000, 0, SuggestionKind::Unplayed), 0);
        // Another click is another draw, not always the same place.
        let draws: std::collections::HashSet<usize> = (0..12)
            .map(|attempt| pick_index(20, 20_000, attempt, SuggestionKind::Forgotten))
            .collect();
        assert!(draws.len() > 3);
    }

    #[test]
    fn the_shortlist_caps_each_artist() {
        let ranked = vec![
            (1, Some(7)),
            (2, Some(7)),
            (3, Some(7)),
            (4, None),
            (5, Some(8)),
        ];
        let picked = shortlist(ranked, |(_, artist)| *artist, 2, 10);
        let ids: Vec<i32> = picked.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![1, 2, 4, 5]);
        let short = shortlist(vec![1, 2, 3], |_| None, 1, 2);
        assert_eq!(short, vec![1, 2]);
    }

    #[test]
    fn a_track_near_the_recent_listening_is_closer() {
        let recent = RecentProfile {
            energy: Some(0.7),
            bpm: Some(124.0),
        };
        let near = vibe_distance(recent, Some(0.65), Some(122.0));
        let far = vibe_distance(recent, Some(0.2), Some(70.0));
        assert!(near < far);
        // A tempo read an octave out is as close as the tempo itself.
        let octave = vibe_distance(recent, Some(0.65), Some(61.0));
        assert!((octave - near).abs() < 0.03);
        // Unknown values count halfway: never ahead of a close match.
        let unknown = vibe_distance(recent, None, None);
        assert!((unknown - 1.0).abs() < f64::EPSILON);
        assert!(near < unknown);
    }
}

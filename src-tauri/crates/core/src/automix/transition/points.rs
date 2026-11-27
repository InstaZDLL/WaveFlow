//! Where a track can be left, and where one can be entered, from the level
//! of each of its beats.
//!
//! The beat list alone says where the beats are, not where the music is: it
//! runs on through a ring-out, and a break before the last section is just
//! more beats. The level of each beat (its RMS from one beat to the next,
//! [`beat_levels`]) shows both: the music stops where the beats fall well
//! under the track's typical level, and a section ends on a single beat that
//! dips under the ones around it. Nothing here knows where bars or phrases
//! start (the tracker does not find downbeats): a phrase is counted in beats
//! from the first beat.

/// The thresholds [`exits`] and [`entries`] work with.
#[derive(Debug, Clone, PartialEq)]
pub struct PointRules {
    /// Beats at the end of the outgoing track its typical level is the
    /// median of.
    pub typical_beats: usize,
    /// How far under that level a beat is past the end of the music, dB.
    pub end_drop_db: f64,
    /// Where, in beats before the end of the music, a break may be.
    pub dip_window: (usize, usize),
    /// How far a beat must dip under the median of the 8 around it to be a
    /// break, dB.
    pub min_dip_db: f64,
    /// Breaks kept, deepest first.
    pub max_dips: usize,
    /// Beats in a phrase.
    pub phrase_beats: usize,
    /// The intro ends on the first beat within this of the incoming track's
    /// typical level (the median over its first 200 beats), dB.
    pub intro_within_db: f64,
    /// How far into the incoming track an entry may be, in beats.
    pub entry_search_beats: usize,
    /// Phrase entries kept, earliest first.
    pub max_entries: usize,
    /// The one beat to enter on instead of the phrases found, to compare
    /// against a recorded transition.
    pub only_entry: Option<usize>,
}

impl Default for PointRules {
    fn default() -> Self {
        Self {
            typical_beats: 64,
            end_drop_db: 6.0,
            dip_window: (8, 32),
            min_dip_db: 3.0,
            max_dips: 3,
            phrase_beats: 32,
            intro_within_db: 3.0,
            entry_search_beats: 128,
            max_entries: 3,
            only_entry: None,
        }
    }
}

/// The level of each beat of `pcm` (interleaved, `channels` per frame, at
/// `rate`), in dB: the RMS over every channel from beat `k` to beat `k + 1`,
/// the last one over one more period.
pub fn beat_levels(pcm: &[f32], channels: usize, rate: u32, beats: &[f64]) -> Vec<f64> {
    let channels = channels.max(1);
    let frames = pcm.len() / channels;
    let rate = f64::from(rate);
    (0..beats.len())
        .map(|k| {
            let a = beats[k];
            let b = beats.get(k + 1).copied().unwrap_or_else(|| {
                let prev = beats[k.saturating_sub(1)];
                a + (a - prev).max(0.3)
            });
            let fa = ((a.max(0.0) * rate) as usize).min(frames);
            let fb = ((b.max(0.0) * rate) as usize).min(frames);
            if fb <= fa {
                return -120.0;
            }
            let sum: f64 = pcm[fa * channels..fb * channels]
                .iter()
                .map(|s| f64::from(*s).powi(2))
                .sum();
            10.0 * (sum / ((fb - fa) * channels) as f64 + 1e-12).log10()
        })
        .collect()
}

/// Median of `v`, `-120` for an empty one.
pub fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return -120.0;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s[s.len() / 2]
}

/// How a way out of the outgoing track starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    /// On a beat that dips under the ones around it: a break, the end of a
    /// section.
    Dip,
    /// No break found: a set number of beats before the end.
    Fallback,
}

/// A way out of the outgoing track. Beats are indices into its beat list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Exit {
    /// The beat the music stops on; the list's length when it runs on to
    /// the last beat.
    pub end: usize,
    /// How far under the typical level the beats from `end` fall, dB; 0
    /// when the music runs on to the last beat.
    pub end_drop_db: f64,
    /// The beat a transition that follows the track's structure starts on.
    pub start: usize,
    pub kind: ExitKind,
    /// How far that beat dips under the ones around it, dB (negative); 0
    /// for a fallback.
    pub dip_db: f64,
}

/// The ways out of a track whose beats have `levels`: where its music
/// stops, then the breaks before that, deepest first, and a fallback
/// `fallback_beats` before the end. Empty for a track too short to judge.
pub fn exits(levels: &[f64], rules: &PointRules, fallback_beats: usize) -> Vec<Exit> {
    let n = levels.len();
    if n < rules.typical_beats.max(rules.dip_window.1 + 1) {
        return Vec::new();
    }
    let typical = median(&levels[n - rules.typical_beats..]);
    let end = (0..n)
        .rev()
        .find(|&k| levels[k] >= typical - rules.end_drop_db)
        .map_or(n, |k| k + 1);
    let end_drop_db = if end < n {
        typical - median(&levels[end..])
    } else {
        0.0
    };
    let dip = |k: usize| -> f64 {
        let around: Vec<f64> = (k.saturating_sub(4)..(k + 5).min(n))
            .filter(|&j| j != k)
            .map(|j| levels[j])
            .collect();
        levels[k] - median(&around)
    };
    let (near, far) = rules.dip_window;
    let mut dips: Vec<(usize, f64)> = (end.saturating_sub(far)..=end.saturating_sub(near))
        .map(|k| (k, dip(k)))
        .filter(|(_, d)| *d <= -rules.min_dip_db)
        .collect();
    dips.sort_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)));
    dips.truncate(rules.max_dips);
    let mut out: Vec<Exit> = dips
        .into_iter()
        .map(|(start, dip_db)| Exit {
            end,
            end_drop_db,
            start,
            kind: ExitKind::Dip,
            dip_db,
        })
        .collect();
    out.push(Exit {
        end,
        end_drop_db,
        start: end.saturating_sub(fallback_beats),
        kind: ExitKind::Fallback,
        dip_db: 0.0,
    });
    out
}

/// A way into the incoming track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entry {
    /// The beat it starts on, or `None` for its first sample, intro and all.
    pub beat: Option<usize>,
    /// The beat its intro ends on.
    pub intro_end: usize,
}

/// The ways into a track whose beats have `levels`: from its first sample,
/// then on each phrase past its intro, earliest first.
pub fn entries(levels: &[f64], rules: &PointRules) -> Vec<Entry> {
    let head = &levels[..levels.len().min(200)];
    let typical = median(head);
    let intro_end = head
        .iter()
        .position(|&l| l >= typical - rules.intro_within_db)
        .unwrap_or(0);
    let mut out = vec![Entry {
        beat: None,
        intro_end,
    }];
    if let Some(k) = rules.only_entry.filter(|k| *k < levels.len()) {
        out.push(Entry {
            beat: Some(k),
            intro_end,
        });
        return out;
    }
    let phrase = rules.phrase_beats.max(1);
    let first = intro_end.div_ceil(phrase) * phrase;
    out.extend(
        (first..rules.entry_search_beats.min(levels.len()))
            .step_by(phrase)
            .take(rules.max_entries)
            .map(|k| Entry {
                beat: Some(k),
                intro_end,
            }),
    );
    out
}

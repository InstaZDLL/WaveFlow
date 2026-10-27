//! Where the beats fall, from the onset envelope and the tempo.
//!
//! The tempo stage gives one period for the track; this stage places the
//! beats. It is the dynamic-programming tracker of Ellis (2007, "Beat
//! Tracking by Dynamic Programming"): every frame gets the best score a
//! chain of beats ending on it can reach, where a chain earns the onset
//! strength under each of its beats and pays for every gap that strays
//! from the period. The best chain, read backwards from the end, is the
//! beat list.
//!
//! The cost of a gap is the square of its log-ratio to the period, so a
//! beat may drift a little early or late — a drummer is not a click —
//! but a gap of half or twice the period costs as much as skipping a beat
//! outright. That is what keeps the chain on the tempo's own metrical
//! level instead of jumping to whichever attack is loudest.

/// How hard a gap that strays from the period is penalised. Higher holds
/// the chain to the tempo; lower lets it follow the attacks. The paper
/// used 100; measured with `analysis_bench --beats` against the Ballroom
/// (698 excerpts) and GTZAN (998) annotations, 400 places the beats best
/// on both, live-played genres included. 800 starts losing them again.
const TIGHTNESS: f64 = 400.0;

/// Beats placed from an onset envelope sampled at `fps` frames per
/// second, at a tempo of `bpm`. Returned as frame indices into `onset`,
/// in order. Empty when the envelope has nothing in it.
pub(crate) fn track(onset: &[f32], fps: f64, bpm: f64) -> Vec<usize> {
    if onset.is_empty() || fps <= 0.0 || bpm <= 0.0 {
        return Vec::new();
    }
    let period = 60.0 * fps / bpm;
    if period < 2.0 || (onset.len() as f64) < 2.0 * period {
        return Vec::new();
    }

    // Scale to unit spread, so the gap penalty means the same thing in a
    // dense mix as in a sparse one.
    let mean = onset.iter().map(|&v| f64::from(v)).sum::<f64>() / onset.len() as f64;
    let spread = (onset
        .iter()
        .map(|&v| (f64::from(v) - mean).powi(2))
        .sum::<f64>()
        / onset.len() as f64)
        .sqrt();
    if spread <= f64::EPSILON {
        return Vec::new();
    }

    // Each frame's own score: the onset strength smoothed over a narrow
    // window (a 32nd of the period each side), so a beat a frame away
    // from its attack still collects it.
    let half = period.round() as isize;
    let kernel: Vec<f64> = (-half..=half)
        .map(|k| (-0.5 * (k as f64 * 32.0 / period).powi(2)).exp())
        .collect();
    let local: Vec<f64> = (0..onset.len() as isize)
        .map(|i| {
            kernel
                .iter()
                .enumerate()
                .filter_map(|(j, &w)| {
                    let at = i + j as isize - half;
                    (at >= 0 && (at as usize) < onset.len())
                        .then(|| w * f64::from(onset[at as usize]) / spread)
                })
                .sum()
        })
        .collect();

    // The chain. A frame looks back between half and twice the period
    // for the predecessor that leaves it the best total.
    let reach_far = (2.0 * period).round() as usize;
    let reach_near = ((period / 2.0).round() as usize).max(1);
    let gap_cost: Vec<f64> = (0..=reach_far)
        .map(|gap| {
            if gap < reach_near {
                f64::NEG_INFINITY
            } else {
                -TIGHTNESS * (gap as f64 / period).ln().powi(2)
            }
        })
        .collect();
    let mut total = vec![0.0f64; onset.len()];
    let mut back = vec![usize::MAX; onset.len()];
    // Until the music starts, a frame is a chain of its own: it would
    // otherwise inherit a predecessor's score from the silence before it.
    let loudest = local.iter().copied().fold(0.0f64, f64::max);
    let mut started = false;
    for t in 0..onset.len() {
        let mut best = f64::NEG_INFINITY;
        let mut from = usize::MAX;
        if started {
            for gap in reach_near..=reach_far.min(t) {
                let s = total[t - gap] + gap_cost[gap];
                if s > best {
                    best = s;
                    from = t - gap;
                }
            }
        }
        if from == usize::MAX {
            total[t] = local[t];
        } else {
            total[t] = local[t] + best;
            back[t] = from;
        }
        if !started && local[t] > 0.01 * loudest {
            started = true;
        }
    }

    // The chain ends on the last frame that is a local peak of the total
    // and holds at least half the typical peak: a fade-out's last faint
    // ticks do not get to end it.
    let peaks: Vec<usize> = (1..total.len().saturating_sub(1))
        .filter(|&i| total[i] > total[i - 1] && total[i] >= total[i + 1])
        .collect();
    if peaks.is_empty() {
        return Vec::new();
    }
    let mut values: Vec<f64> = peaks.iter().map(|&i| total[i]).collect();
    values.sort_by(f64::total_cmp);
    let median = values[values.len() / 2];
    let Some(&end) = peaks.iter().rev().find(|&&i| total[i] >= 0.5 * median) else {
        return Vec::new();
    };

    let mut beats = vec![end];
    let mut at = end;
    while back[at] != usize::MAX {
        at = back[at];
        beats.push(at);
    }
    beats.reverse();
    trim(&mut beats, &local);
    beats
}

/// The first byte of an encoded beat list: the layout below. A reader
/// meeting another value has a list from a later build and leaves it.
const FORMAT: u8 = 1;

/// A beat list as `track_analysis.beats` stores it: [`FORMAT`], the
/// first beat in milliseconds (`u32`), then each gap to the next in
/// milliseconds (`u16`), little-endian. About 1 KB for four minutes at
/// 120 BPM. Times are rounded to the millisecond before the gaps are
/// taken, so the rounding never adds up along the track. `None` for an
/// empty list.
pub fn encode(beats: &[f64]) -> Option<Vec<u8>> {
    let ms: Vec<u64> = beats
        .iter()
        .map(|b| (b.max(0.0) * 1000.0).round() as u64)
        .collect();
    let (&first, rest) = ms.split_first()?;
    let mut out = Vec::with_capacity(5 + 2 * rest.len());
    out.push(FORMAT);
    out.extend_from_slice(&u32::try_from(first).ok()?.to_le_bytes());
    let mut prev = first;
    for &b in rest {
        // The slowest tempo searched leaves 1.5 s between beats; a gap a
        // `u16` cannot hold is not one this tracker produces.
        let gap = u16::try_from(b.checked_sub(prev)?).ok()?;
        out.extend_from_slice(&gap.to_le_bytes());
        prev = b;
    }
    Some(out)
}

/// The beats in seconds from an [`encode`]d list, `None` when the bytes
/// are not one.
pub fn decode(bytes: &[u8]) -> Option<Vec<f64>> {
    let (&format, rest) = bytes.split_first()?;
    if format != FORMAT || rest.len() < 4 || (rest.len() - 4) % 2 != 0 {
        return None;
    }
    let mut at = u64::from(u32::from_le_bytes(rest[..4].try_into().ok()?));
    let mut out = vec![at as f64 / 1000.0];
    for gap in rest[4..].chunks_exact(2) {
        at += u64::from(u16::from_le_bytes([gap[0], gap[1]]));
        out.push(at as f64 / 1000.0);
    }
    Some(out)
}

/// Drop the beats at either end that sit on next to nothing: the chain
/// runs on through an intro or an outro the music has not reached yet.
fn trim(beats: &mut Vec<usize>, local: &[f64]) {
    if beats.len() < 3 {
        return;
    }
    let strength: Vec<f64> = beats.iter().map(|&b| local[b]).collect();
    let rms = (strength.iter().map(|s| s * s).sum::<f64>() / strength.len() as f64).sqrt();
    let floor = 0.5 * rms;
    let first = strength.iter().position(|&s| s >= floor).unwrap_or(0);
    let last = strength
        .iter()
        .rposition(|&s| s >= floor)
        .unwrap_or(beats.len() - 1);
    beats.truncate(last + 1);
    beats.drain(..first);
}

#[cfg(test)]
mod tests;

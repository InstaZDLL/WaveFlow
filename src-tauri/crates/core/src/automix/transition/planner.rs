//! Choosing a transition for a pair of tracks, or choosing not to mix.
//!
//! For every template, every way out of the outgoing track and every way
//! into the incoming one ([`super::points`]), the planner lays the
//! transition out ([`Layout`]) or says why it cannot, and scores what it
//! could lay out on what the analysis measures today:
//!
//! - **grid** — how steady both beat grids are where they are used;
//! - **tempo** — the gap the stretch has to make up, or the drift left when
//!   nothing is stretched;
//! - **exit** — how clearly the outgoing music stops, and, for a template
//!   that follows the structure, how deep the break it starts on is;
//! - **entry** — an intro playing under the transition, or a phrase entry
//!   that skips much of the incoming track, cost a little;
//! - **energy** — how close the two tracks' levels are, each against its own
//!   typical level: the outgoing one over the transition, the incoming one
//!   over as many beats from where it takes over.
//!
//! The best candidate is kept if its score reaches the threshold; otherwise
//! the pair is not mixed and the player makes the transition it makes
//! without AutoMix. The same pair always gets the same answer: nothing is
//! random, and ties go to the earlier template, exit and entry.

use super::super::{Grid, FIT_BEATS, MAX_SCATTER, MIN_GRID_BEATS};
use super::points::{self, Entry, Exit, ExitKind, PointRules};
use super::template::{Effect, EntryRule, Length, Stretch, Template};
use super::tempo::Glide;

/// A track as the planner sees it.
#[derive(Debug, Clone, Copy)]
pub struct Track<'a> {
    /// Where its beats fall, in seconds.
    pub beats: &'a [f64],
    /// The level of each beat ([`points::beat_levels`]).
    pub levels: &'a [f64],
    pub duration: f64,
}

/// The weights of the score's parts, and the score a candidate needs.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanRules {
    pub points: PointRules,
    pub grid: f64,
    pub tempo: f64,
    pub exit: f64,
    pub entry: f64,
    pub energy: f64,
    /// Under this, the pair is not mixed.
    pub threshold: f64,
}

impl Default for PlanRules {
    fn default() -> Self {
        Self {
            points: PointRules::default(),
            grid: 0.25,
            tempo: 0.2,
            exit: 0.25,
            entry: 0.15,
            energy: 0.15,
            threshold: 0.6,
        }
    }
}

/// A transition laid out. Times are output seconds from the start of the
/// outgoing track's playback, which is its own time while it is not
/// stretched; "virtual" outgoing time runs on through the loops an
/// [`Effect::Loop`] adds after the end of its music.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// When the incoming track's first sample plays; the transition starts.
    pub fade_start: f64,
    /// When it ends, the outgoing track cut.
    pub fade_end: f64,
    /// The outgoing beat, in output seconds, over the transition.
    pub period: f64,
    /// Its length in outgoing beats.
    pub beats: u32,
    /// How many outgoing beats an incoming one spans: 0.5, 1 or 2.
    pub fold: f64,
    /// Incoming period over the outgoing one, after folding.
    pub ratio: f64,
    /// Each side's speed over its own (virtual, for the outgoing) time.
    pub out_glide: Glide,
    pub in_glide: Glide,
    /// Where the incoming track is played from, in its own seconds.
    pub in_from: f64,
    /// Where the outgoing music ends, in its own seconds: loops start there.
    pub out_end: f64,
    /// The outgoing beat in its own seconds, for loops.
    pub out_period: f64,
    /// When the outgoing bass is cut, if the template cuts it.
    pub bass_cut_at: Option<f64>,
    /// When the incoming tempo is back to its own.
    pub settled: f64,
    /// The furthest the two grids put a pair of beats apart over the
    /// transition, in milliseconds (0 when one side is stretched onto the
    /// other).
    pub drift_ms: f64,
}

/// The parts of a candidate's score, each 0 to 1, and their weighted sum
/// with the template's prior.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    pub grid: f64,
    pub tempo: f64,
    pub exit: f64,
    pub entry: f64,
    pub energy: f64,
    pub total: f64,
}

/// A transition the planner could lay out.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Index into the templates given.
    pub template: usize,
    pub exit: Exit,
    pub entry: Entry,
    pub layout: Layout,
    pub score: Score,
}

/// One it could not, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Rejected {
    pub template: usize,
    pub exit: Option<Exit>,
    pub entry: Option<Entry>,
    pub reason: String,
}

/// What the planner made of a pair.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// Best first.
    pub candidates: Vec<Candidate>,
    pub rejected: Vec<Rejected>,
    /// The index of the candidate kept, or why the pair is not mixed.
    pub choice: Result<usize, String>,
}

/// Plan the transition from `out` to `inc` within `cap` seconds.
pub fn plan(
    out: Track<'_>,
    inc: Track<'_>,
    cap: f64,
    templates: &[Template],
    rules: &PlanRules,
) -> Plan {
    let mut candidates = Vec::new();
    let mut rejected = Vec::new();
    let entries = points::entries(inc.levels, &rules.points);
    for (ti, t) in templates.iter().enumerate() {
        let fallback = match t.length {
            Length::Structure { bar, .. } => 4 * bar as usize,
            Length::Beats(_) => 0,
        };
        let exits = points::exits(out.levels, &rules.points, fallback);
        if exits.is_empty() {
            rejected.push(Rejected {
                template: ti,
                exit: None,
                entry: None,
                reason: "outgoing track too short to judge".into(),
            });
            continue;
        }
        // A template of fixed lengths only reads where the music ends: its
        // exits differ by their start alone, so one is enough.
        let exits: Vec<Exit> = match t.length {
            Length::Beats(_) => exits.into_iter().take(1).collect(),
            Length::Structure { .. } => exits,
        };
        for exit in &exits {
            if t.needs_break && exit.kind != ExitKind::Dip {
                rejected.push(Rejected {
                    template: ti,
                    exit: Some(*exit),
                    entry: None,
                    reason: "no break to start on".into(),
                });
                continue;
            }
            for entry in entries.iter().filter(|e| match t.entry {
                EntryRule::TrackStart => e.beat.is_none(),
                EntryRule::Phrase => e.beat.is_some(),
            }) {
                match lay_out(t, out, inc, exit, entry, cap) {
                    Ok((layout, og, ig)) => {
                        let score = score(t, out, inc, exit, entry, &layout, &og, &ig, rules);
                        candidates.push(Candidate {
                            template: ti,
                            exit: *exit,
                            entry: *entry,
                            layout,
                            score,
                        });
                    }
                    Err(reason) => rejected.push(Rejected {
                        template: ti,
                        exit: Some(*exit),
                        entry: Some(*entry),
                        reason,
                    }),
                }
            }
        }
    }
    // Stable: equal scores keep the template, exit, entry order.
    candidates.sort_by(|a, b| b.score.total.total_cmp(&a.score.total));
    let choice = match candidates.first() {
        Some(c) if c.score.total >= rules.threshold => Ok(0),
        Some(c) => Err(format!(
            "best candidate scores {:.3}, under {:.3}",
            c.score.total, rules.threshold
        )),
        None => Err("no template could be laid out".into()),
    };
    Plan {
        candidates,
        rejected,
        choice,
    }
}

fn steady(g: &Grid) -> bool {
    g.scatter <= MAX_SCATTER * g.period
}

/// The incoming period folded onto the outgoing one by ×½, ×1 or ×2,
/// whichever is nearest on a log scale: `(fold, ratio)`.
fn fold(og: &Grid, ig: &Grid) -> (f64, f64) {
    let q = ig.period / og.period;
    let fold = [0.5, 1.0, 2.0]
        .into_iter()
        .min_by(|a: &f64, b: &f64| (q / a).ln().abs().total_cmp(&(q / b).ln().abs()))
        .unwrap_or(1.0);
    (fold, q / fold)
}

fn lay_out(
    t: &Template,
    out: Track<'_>,
    inc: Track<'_>,
    exit: &Exit,
    entry: &Entry,
    cap: f64,
) -> Result<(Layout, Grid, Grid), String> {
    // The outgoing grid over the beats before the end of its music.
    if exit.end < MIN_GRID_BEATS {
        return Err("outgoing: too few beats before the end of the music".into());
    }
    let run = exit.end.min(FIT_BEATS);
    let og = Grid::fit(&out.beats[exit.end - run..exit.end]).ok_or("outgoing: no grid")?;
    if !steady(&og) {
        return Err("outgoing: unsteady grid".into());
    }
    let end_line = run as f64;
    let out_end = og.at(end_line).min(out.duration);

    // The incoming grid over the beats from the entry.
    let k0 = entry.beat.unwrap_or(0);
    if k0 + MIN_GRID_BEATS > inc.beats.len() {
        return Err("incoming: too few beats after the entry".into());
    }
    let ig = Grid::fit(&inc.beats[k0..(k0 + FIT_BEATS).min(inc.beats.len())])
        .ok_or("incoming: no grid")?;
    if !steady(&ig) {
        return Err("incoming: unsteady grid".into());
    }
    let in_from = if entry.beat.is_some() {
        ig.at(0.0)
    } else {
        0.0
    };
    let (fold, ratio) = fold(&og, &ig);
    let stretched = !matches!(t.stretch, Stretch::None { .. });
    if stretched && ratio.ln().abs() > t.max_gap {
        return Err(format!(
            "tempo gap {:+.1} %, over the template's {:.1} %",
            (ratio - 1.0) * 100.0,
            (t.max_gap.exp() - 1.0) * 100.0
        ));
    }

    // Loops carry the outgoing grid on past the end of the music.
    let looped: f64 = match &t.effect {
        Effect::Loop { lengths, .. } => lengths.iter().sum(),
        _ => 0.0,
    };
    let last_line = end_line + looped;

    let lengths: Vec<u32> = match &t.length {
        Length::Beats(list) => list.clone(),
        Length::Structure { bar, min, max } => {
            let bar = (*bar).max(1);
            let from_exit = (exit.end - exit.start.min(exit.end)) as u32 + looped as u32;
            let mut list = vec![from_exit.min(*max)];
            // Shorter, in whole bars, should the crossfade length need it.
            let mut n = from_exit.min(*max) / bar * bar;
            while n >= (*min).max(1) {
                if n != list[0] {
                    list.push(n);
                }
                n -= bar;
            }
            list.retain(|n| *n >= *min);
            list
        }
    };
    if lengths.is_empty() {
        return Err("the break is too close to the end".into());
    }

    let step = fold.max(1.0);
    let mut last_err = String::from("nothing fits the crossfade length");
    for n in lengths {
        let nf = f64::from(n);
        let first = last_line - nf;
        if og.at(first) < 0.0 {
            continue;
        }
        let out_glide = match t.stretch {
            Stretch::Outgoing {
                glide_beats,
                lead_beats,
            } => {
                let to = og.at(first - lead_beats).max(0.0);
                let from = og.at(first - lead_beats - glide_beats).max(0.0);
                Glide {
                    from: 1.0,
                    to: 1.0 / ratio,
                    start: from,
                    end: to.max(from + 1e-3),
                }
            }
            _ => Glide::constant(1.0),
        };
        let out_t = |s: f64| out_glide.output_time(0.0, s);
        let in_speed = match t.stretch {
            Stretch::Incoming { .. } => ratio,
            _ => 1.0,
        };
        let m = ((nf / 2.0) / step).round() * step;
        let anchor = out_t(og.at(first + m));
        let fade_start = anchor - (ig.at(m / fold) - in_from) / in_speed;
        let fade_end = out_t(og.at(last_line).min(out_end + looped * og.period));
        if fade_start < 0.0 {
            last_err = "the incoming intro starts before the outgoing track".into();
            continue;
        }
        if fade_end - fade_start > cap + 1e-9 {
            last_err = format!(
                "{n} beats take {:.1} s, over the {cap:.1} s crossfade length",
                fade_end - fade_start
            );
            continue;
        }
        let period = (fade_end - out_t(og.at(first))) / nf;
        let drift_ms = if stretched {
            0.0
        } else {
            drift(&og, &ig, first, n, fold, m, fade_start, in_from) * 1000.0
        };
        if let Stretch::None { max_drift_ms } = t.stretch {
            if drift_ms > max_drift_ms {
                last_err = format!(
                    "the beats drift {drift_ms:.0} ms over {n} beats (tempo gap {:+.1} %)",
                    (ratio - 1.0) * 100.0
                );
                continue;
            }
        }
        let (in_glide, settled) = match t.stretch {
            Stretch::Incoming { back_beats } => {
                let at_end = in_from + (fade_end - fade_start) * ratio;
                let back = at_end + back_beats * ig.period;
                let g = Glide {
                    from: ratio,
                    to: 1.0,
                    start: at_end,
                    end: back,
                };
                (g, fade_start + g.output_time(in_from, back))
            }
            _ => (Glide::constant(1.0), fade_end),
        };
        let bass_cut_at = t.bass_cut.map(|c| fade_end - c.beats_before_end * period);
        return Ok((
            Layout {
                fade_start,
                fade_end,
                period,
                beats: n,
                fold,
                ratio,
                out_glide,
                in_glide,
                in_from,
                out_end,
                out_period: og.period,
                bass_cut_at,
                settled,
                drift_ms,
            },
            og,
            ig,
        ));
    }
    Err(last_err)
}

/// The furthest apart the two grids put a pair of beats that should meet,
/// over the `n` outgoing lines from `first`, neither side stretched: each
/// line's partner counted from the anchor `m` lines in.
#[allow(clippy::too_many_arguments)]
fn drift(
    og: &Grid,
    ig: &Grid,
    first: f64,
    n: u32,
    fold: f64,
    m: f64,
    fade_start: f64,
    in_from: f64,
) -> f64 {
    let mut worst: f64 = 0.0;
    for k in 0..=n {
        let j = m / fold + (f64::from(k) - m) / fold;
        if j < 0.0 || j.fract() != 0.0 {
            continue;
        }
        let incoming = fade_start + ig.at(j) - in_from;
        worst = worst.max((incoming - og.at(first + f64::from(k))).abs());
    }
    worst
}

/// The median level of `levels[a..b]` against the track's typical level.
fn relative(levels: &[f64], a: usize, b: usize, typical: f64) -> f64 {
    let b = b.min(levels.len());
    if a >= b {
        return 0.0;
    }
    points::median(&levels[a..b]) - typical
}

#[allow(clippy::too_many_arguments)]
fn score(
    t: &Template,
    out: Track<'_>,
    inc: Track<'_>,
    exit: &Exit,
    entry: &Entry,
    layout: &Layout,
    og: &Grid,
    ig: &Grid,
    rules: &PlanRules,
) -> Score {
    let unit = |x: f64| x.clamp(0.0, 1.0);
    let grid = unit(1.0 - (og.scatter / og.period).max(ig.scatter / ig.period) / MAX_SCATTER);
    let tempo = match t.stretch {
        Stretch::None { max_drift_ms } => unit(1.0 - layout.drift_ms / max_drift_ms),
        _ => unit(1.0 - 0.5 * layout.ratio.ln().abs() / t.max_gap),
    };
    // A music end the levels show beats one the beat list merely runs out
    // on; a deep break beats a set count.
    let end = if exit.end < out.levels.len() {
        unit(exit.end_drop_db / 15.0)
    } else {
        0.5
    };
    let exit_score = match t.length {
        Length::Beats(_) => end,
        Length::Structure { .. } => {
            let start = match exit.kind {
                ExitKind::Dip => unit(-exit.dip_db / 9.0),
                ExitKind::Fallback => 0.3,
            };
            0.5 * end + 0.5 * start
        }
    };
    let entry_score = match entry.beat {
        // The intro plays under the transition: the longer, the worse.
        None => {
            let intro = inc.beats.get(entry.intro_end).copied().unwrap_or(0.0);
            unit(1.0 - 0.5 * intro / (layout.fade_end - layout.fade_start).max(1.0))
        }
        // Past the intro on a phrase; skipping much of the track costs.
        Some(k) => unit(1.0 - 0.3 * k as f64 / rules.points.entry_search_beats.max(1) as f64),
    };
    let n = layout.beats as usize;
    let out_typical =
        points::median(&out.levels[out.levels.len().saturating_sub(rules.points.typical_beats)..]);
    let in_typical = points::median(&inc.levels[..inc.levels.len().min(200)]);
    // The outgoing track over the transition, against the incoming one
    // over as many beats from where it takes over: the end of the
    // transition, not its intro.
    let out_rel = relative(
        out.levels,
        exit.end.saturating_sub(n),
        exit.end,
        out_typical,
    );
    let in_speed = match t.stretch {
        Stretch::Incoming { .. } => layout.ratio,
        _ => 1.0,
    };
    let taken_over = layout.in_from + (layout.fade_end - layout.fade_start) * in_speed;
    let k_after = inc.beats.partition_point(|b| *b < taken_over);
    let in_rel = relative(inc.levels, k_after, k_after + n.max(8), in_typical);
    let energy = unit(1.0 - (out_rel - in_rel).abs() / 12.0);
    let total = rules.grid * grid
        + rules.tempo * tempo
        + rules.exit * exit_score
        + rules.entry * entry_score
        + rules.energy * energy
        + t.prior;
    Score {
        grid,
        tempo,
        exit: exit_score,
        entry: entry_score,
        energy,
        total,
    }
}

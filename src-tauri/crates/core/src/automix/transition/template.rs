//! Templates: a transition as a combination of bricks, every number a field.
//!
//! [`builtin`] lists the templates the planner tries. Each is data: which
//! side is stretched and how, how long the transition is, the gains, the
//! bass change, an effect, where the incoming track may be entered. The
//! outgoing track is always cut where the transition ends — the end of its
//! music ([`super::points::Exit::end`]) — so what it would ring out with
//! after that is dropped.

use super::envelope::{Ramp, Shape};

/// Which track is stretched, and how.
#[derive(Debug, Clone, PartialEq)]
pub enum Stretch {
    /// Neither: the beats are lined up in the middle and drift apart by the
    /// tempo gap; the transition is shortened until its ends drift no more
    /// than `max_drift_ms`.
    None { max_drift_ms: f64 },
    /// The incoming track plays at the outgoing tempo through the
    /// transition, then glides back to its own over `back_beats` of its
    /// beats.
    Incoming { back_beats: f64 },
    /// The outgoing track glides to the incoming tempo over `glide_beats`,
    /// ending `lead_beats` before the transition, and stays there; the
    /// incoming track plays as it is.
    Outgoing { glide_beats: f64, lead_beats: f64 },
}

/// How long the transition is, in outgoing beats.
#[derive(Debug, Clone, PartialEq)]
pub enum Length {
    /// The longest of these that fits in the crossfade length (the incoming
    /// track's intro included when it starts from its first sample).
    Beats(Vec<u32>),
    /// From the exit's start to the end of the music, as the track's
    /// structure has it; shortened to whole bars of `bar` beats when longer
    /// than `max` or than the crossfade length, refused under `min`.
    Structure { bar: u32, min: u32, max: u32 },
}

/// The gains of the two sides over the transition.
#[derive(Debug, Clone, PartialEq)]
pub enum Gains {
    /// One gain per side: the outgoing one falls along `out`, the incoming
    /// one rises along `inc`.
    Whole { out: Ramp, inc: Ramp },
    /// The signal split at `cutoffs` (Hz, [`super::bands::BandSplit`]), one
    /// ramp per band and side, lowest band first.
    Bands {
        cutoffs: Vec<f64>,
        out: Vec<Ramp>,
        inc: Vec<Ramp>,
    },
}

/// The outgoing bass cut on a beat rather than faded: the bands up to
/// `top_band` go to silence over `fade_s` on the line `beats_before_end`
/// beats before the end of the transition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BassCut {
    pub top_band: usize,
    pub beats_before_end: f64,
    pub fade_s: f64,
}

/// What is done to the outgoing track on its way out.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    None,
    /// Its last `send_beats` beats feed an echo `delay_beats` long, each
    /// repeat `feedback` times the one before through `low_cut..high_cut`
    /// Hz, mixed in at `wet`; it rings on after the cut.
    Echo {
        delay_beats: f64,
        feedback: f32,
        wet: f32,
        send_beats: f64,
        low_cut: f64,
        high_cut: f64,
    },
    /// After the end of its music, loops of its last beats
    /// ([`super::fx::loop_segments`]) carry on: the transition runs over
    /// them too.
    Loop {
        back_beats: f64,
        lengths: Vec<f64>,
    },
    /// A high-pass on it rising over `out_high_pass` Hz, a low-pass on the
    /// incoming track opening over `in_low_pass` Hz along `in_open`.
    Sweep {
        out_high_pass: (f64, f64),
        in_low_pass: (f64, f64),
        in_open: Ramp,
        q: f64,
    },
}

/// Where the incoming track may be entered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryRule {
    /// From its first sample, intro and all.
    TrackStart,
    /// On a phrase past its intro.
    Phrase,
}

/// A transition as data.
#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    pub name: &'static str,
    pub stretch: Stretch,
    /// The widest tempo gap a stretching template takes, as `|ln ratio|`.
    pub max_gap: f64,
    pub length: Length,
    pub gains: Gains,
    pub bass_cut: Option<BassCut>,
    pub effect: Effect,
    pub entry: EntryRule,
    /// How much of the overlap's extra power is taken back
    /// ([`super::level::keep_power`]), 0 to 1.
    pub headroom: f64,
    /// Only on an exit that starts on a break.
    pub needs_break: bool,
    /// Added to the score: a template preferred when the pair suits several.
    pub prior: f64,
}

/// The widest tempo gap the stretching templates take: 8 %.
pub fn stretch_gap() -> f64 {
    1.08_f64.ln()
}

const FADE_BEATS: [u32; 4] = [32, 16, 8, 4];

/// A plain template: whole-signal equal-power fade, entering from the first
/// sample, nothing else.
fn plain(name: &'static str, stretch: Stretch) -> Template {
    Template {
        name,
        stretch,
        max_gap: stretch_gap(),
        length: Length::Beats(FADE_BEATS.to_vec()),
        gains: Gains::Whole {
            out: Ramp::whole(),
            inc: Ramp::whole(),
        },
        bass_cut: None,
        effect: Effect::None,
        entry: EntryRule::TrackStart,
        headroom: 0.0,
        needs_break: false,
        prior: 0.0,
    }
}

/// The band swap, set as in the reference transition AM-001: the outgoing
/// track glides to the incoming tempo 64 beats ahead, the incoming one comes
/// in on a phrase at a break and plays as it is, and the two hand over in
/// four bands, highs first, the outgoing bass cut on the last beat.
///
/// AM-001, measured on a recording (sample-level alignment, 0.99 waveform
/// correlation with both files): a 9 s swap over 19 beats at 126 BPM, from
/// a one-beat break to where the outgoing music stops; the outgoing track
/// time-stretched by 0.8 % with its pitch kept, about 30 s ahead; beats of
/// the two within 6 ms. Band gains, as fractions of the swap (when each
/// band is done moving): over 2.5 kHz both move over 0–0.44; 600 Hz–2.5 kHz
/// in over 0.17–0.44, out over 0.72–1; 150–600 Hz in over 0.50–0.78, out
/// over 0.94–1; under 150 Hz in over 0.61–0.89, out cut on the last beat.
/// The overall level never drops. One transition, at close tempos: the
/// values are a setting, not a rule.
pub fn band_swap() -> Template {
    let s = |a, b| Ramp::new(a, b, Shape::Smooth);
    Template {
        name: "band-swap",
        stretch: Stretch::Outgoing {
            glide_beats: 8.0,
            lead_beats: 64.0,
        },
        max_gap: stretch_gap(),
        length: Length::Structure {
            bar: 4,
            min: 8,
            max: 32,
        },
        gains: Gains::Bands {
            cutoffs: vec![150.0, 600.0, 2500.0],
            // The bass does not fade: `bass_cut` takes it on the beat.
            out: vec![s(1.0, 1.0), s(0.94, 1.0), s(0.72, 1.0), s(0.0, 0.44)],
            inc: vec![s(0.61, 0.89), s(0.50, 0.78), s(0.17, 0.44), s(0.0, 0.44)],
        },
        bass_cut: Some(BassCut {
            top_band: 0,
            beats_before_end: 1.0,
            fade_s: 0.01,
        }),
        effect: Effect::None,
        entry: EntryRule::Phrase,
        headroom: 1.0,
        needs_break: false,
        prior: 0.05,
    }
}

/// Every template the planner knows, in the order its ties are broken.
pub fn builtin() -> Vec<Template> {
    let p = |a, b| Ramp::new(a, b, Shape::EqualPower);
    let tempo_in = || Stretch::Incoming { back_beats: 16.0 };
    let mut echo = plain("echo", tempo_in());
    // The outgoing track only comes down 4.5 dB before it is cut on its
    // last beat line, which feeds the echo.
    echo.gains = Gains::Whole {
        out: p(0.0, 1.0 / 0.6),
        inc: p(0.0, 0.75),
    };
    echo.effect = Effect::Echo {
        delay_beats: 0.75,
        feedback: 0.6,
        wet: 0.8,
        send_beats: 1.0,
        low_cut: 300.0,
        high_cut: 5000.0,
    };
    let looping = |name, lengths: Vec<f64>| {
        let mut t = plain(name, tempo_in());
        t.gains = Gains::Whole {
            out: Ramp::new(0.5, 1.0, Shape::Smooth),
            inc: p(0.0, 0.6),
        };
        t.effect = Effect::Loop {
            back_beats: 2.0,
            lengths,
        };
        t
    };
    let mut sweep = plain("filter", tempo_in());
    sweep.gains = Gains::Whole {
        out: p(0.4, 1.0),
        inc: p(0.0, 0.6),
    };
    sweep.effect = Effect::Sweep {
        out_high_pass: (20.0, 1200.0),
        in_low_pass: (300.0, 20_000.0),
        in_open: Ramp::new(0.0, 0.8, Shape::Linear),
        q: 0.9,
    };
    vec![
        {
            let mut t = plain(
                "current",
                Stretch::None {
                    max_drift_ms: super::super::MAX_DRIFT_MS,
                },
            );
            t.length = Length::Beats(vec![32, 16, 8, 4, 2, 1]);
            t
        },
        plain("tempo-in", tempo_in()),
        plain(
            "tempo-out",
            Stretch::Outgoing {
                glide_beats: 8.0,
                lead_beats: 0.0,
            },
        ),
        echo,
        looping("loop", vec![2.0; 4]),
        looping(
            "stutter",
            vec![2.0, 2.0, 1.0, 1.0, 0.5, 0.5, 0.25, 0.25, 0.25, 0.25],
        ),
        sweep,
        band_swap(),
    ]
}

use super::bands::{band_powers, BandGains};
use super::envelope::{Ramp, Shape};
use super::fx::{loop_segments, Echo};
use super::level::{keep_power, Limiter};
use super::planner::{plan, PlanRules, Track};
use super::points::{entries, exits, ExitKind, PointRules};
use super::template::{builtin, Stretch};
use super::tempo::Glide;

const RATE: u32 = 48_000;

/// A deterministic noise in -1..1, the same every run.
fn noise(n: usize) -> Vec<f32> {
    let mut x: u32 = 0x1234_5678;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (f64::from(x) / f64::from(u32::MAX) * 2.0 - 1.0) as f32
        })
        .collect()
}

const CUTS: [f64; 3] = [150.0, 600.0, 2500.0];

#[test]
fn at_full_gain_the_bands_give_back_the_signal() {
    let x = noise(2 * RATE as usize);
    let mut bands = BandGains::new(&CUTS, 2, RATE);
    let latency = bands.latency();
    let mut out = vec![0.0_f32; x.len()];
    for (i, o) in x.chunks(2).zip(out.chunks_mut(2)) {
        bands.frame(i, o);
    }
    let worst = out[latency * 2..]
        .iter()
        .zip(&x)
        .map(|(o, i)| (o - i).abs())
        .fold(0.0_f32, f32::max);
    assert!(worst < 2e-5, "{worst}");
}

/// Energy of a tone that comes out with only `band` at full gain, as a
/// share of what comes out with every band at full gain.
fn kept(freq: f64, band: usize) -> f64 {
    let run = |gains: &[f32]| {
        let mut b = BandGains::new(&CUTS, 1, RATE);
        b.set_gains(gains);
        let mut e = 0.0;
        let mut y = [0.0];
        for i in 0..2 * RATE as usize {
            let x = (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(RATE)).sin() as f32;
            b.frame(&[x], &mut y);
            if i >= RATE as usize {
                e += f64::from(y[0]).powi(2);
            }
        }
        e
    };
    let mut only = [0.0; 4];
    only[band] = 1.0;
    run(&only) / run(&[1.0; 4])
}

#[test]
fn a_tone_stays_in_its_band() {
    for (freq, band) in [(40.0, 0), (300.0, 1), (1200.0, 2), (8000.0, 3)] {
        let k = kept(freq, band);
        assert!(k > 0.99, "{freq} Hz keeps {k:.4} in band {band}");
    }
    // Right on a cut-off, half each side.
    let k = kept(600.0, 1);
    assert!((k - 0.25).abs() < 0.05, "{k}");
}

#[test]
fn band_powers_say_where_the_energy_is() {
    let tone: Vec<f32> = (0..RATE as usize)
        .map(|i| (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / f64::from(RATE)).sin() as f32)
        .collect();
    let p = band_powers(&tone, 1, RATE, &CUTS, 0, tone.len());
    let total: f64 = p.iter().sum();
    assert!(p[2] / total > 0.99, "{p:?}");
    // A sine of amplitude 1 has a mean square of 0.5.
    assert!((total - 0.5).abs() < 0.02, "{total}");
}

#[test]
fn ramps_start_and_end_where_they_say() {
    for shape in [Shape::EqualPower, Shape::Smooth, Shape::Linear] {
        let r = Ramp::new(0.2, 0.6, shape);
        assert_eq!(r.rise(0.0), 0.0);
        assert_eq!(r.rise(0.2), 0.0);
        assert!((r.rise(0.6) - 1.0).abs() < 1e-12);
        assert!((r.fall(0.0) - 1.0).abs() < 1e-12);
        assert!(r.fall(1.0).abs() < 1e-12);
        let mut last = 0.0;
        for k in 0..=100 {
            let g = r.rise(f64::from(k) / 100.0);
            assert!(g >= last - 1e-12, "{shape:?} goes back down");
            last = g;
        }
    }
    let r = Ramp::whole();
    for k in 0..=10 {
        let u = f64::from(k) / 10.0;
        assert!((r.rise(u).powi(2) + r.fall(u).powi(2) - 1.0).abs() < 1e-12);
    }
}

#[test]
fn a_glide_takes_the_time_its_speeds_add_up_to() {
    let steady = Glide::constant(1.25);
    assert!((steady.output_time(0.0, 10.0) - 8.0).abs() < 1e-12);

    let g = Glide {
        from: 1.0,
        to: 1.08,
        start: 10.0,
        end: 20.0,
    };
    // The same integral at a thousandth of the step.
    let fine: f64 = (0..10_000_000)
        .map(|k| 1e-6 / g.speed(10.0 + (k as f64 + 0.5) * 1e-6))
        .sum();
    let expected = 10.0 + fine + 10.0 / 1.08;
    assert!((g.output_time(0.0, 30.0) - expected).abs() < 1e-6);
    // And back.
    for t in [3.0, 12.5, 18.0, 27.0] {
        let s = g.track_time(0.0, t);
        assert!((g.output_time(0.0, s) - t).abs() < 1e-9, "{t}");
    }
}

#[test]
fn an_echo_repeats_on_its_delay() {
    let delay = 1000;
    let mut echo = Echo::new(delay, 1, RATE, 0.5, 20.0, 20_000.0);
    let mut out = Vec::new();
    let mut wet = [0.0];
    for i in 0..4 * delay {
        let send = if i == 0 { 1.0 } else { 0.0 };
        echo.frame(&[send], &mut wet);
        out.push(wet[0].abs());
    }
    let peak_in = |a: usize, b: usize| (a..b).max_by(|x, y| out[*x].total_cmp(&out[*y])).unwrap();
    let first = peak_in(1, delay + delay / 2);
    let second = peak_in(delay + delay / 2, 2 * delay + delay / 2);
    // The band-pass smears the impulse by a few samples, no more.
    assert!(first.abs_diff(delay) < 8, "{first}");
    assert!(second.abs_diff(2 * delay) < 16, "{second}");
    assert!(out[second] < out[first] * 0.6 && out[second] > out[first] * 0.3);
}

#[test]
fn the_limiter_leaves_what_is_under_and_counts_what_is_not() {
    let mut lim = Limiter::new(2, RATE, 0.98, 0.002, 0.05);
    let latency = lim.latency();
    let x: Vec<f32> = noise(20_000).iter().map(|s| s * 0.5).collect();
    let mut out = Vec::new();
    for f in x.chunks(2) {
        let mut io = [f[0], f[1]];
        lim.frame(&mut io);
        out.extend_from_slice(&io);
    }
    assert_eq!(lim.touched(), 0);
    assert_eq!(&out[latency * 2..], &x[..x.len() - latency * 2]);

    // A burst at twice full scale: nothing comes out over the ceiling.
    let mut lim = Limiter::new(1, RATE, 0.98, 0.002, 0.05);
    let mut worst: f32 = 0.0;
    for i in 0..10_000 {
        let mut io = [if (4000..4100).contains(&i) { 2.0 } else { 0.3 }];
        lim.frame(&mut io);
        worst = worst.max(io[0].abs());
    }
    assert!(worst <= 0.98 + 1e-6, "{worst}");
    assert!(lim.touched() >= 100, "{}", lim.touched());
}

#[test]
fn the_overlap_is_turned_down_by_the_power_it_adds() {
    let p = [1.0];
    // An equal-power fade keeps the power: nothing to take back.
    let a = std::f64::consts::FRAC_PI_4;
    let g = keep_power(&[a.cos() as f32], &[a.sin() as f32], &p, &p, 1.0, 0.25);
    assert!((g - 1.0).abs() < 1e-6);
    // Both at full: twice the power, so 3 dB down.
    let g = keep_power(&[1.0], &[1.0], &p, &p, 1.0, 0.25);
    assert!((g - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    // Not at all at strength 0, and never under the floor.
    assert_eq!(keep_power(&[1.0], &[1.0], &p, &p, 0.0, 0.25), 1.0);
    let g = keep_power(&[1.0; 4], &[1.0; 4], &[1.0; 4], &[1.0; 4], 1.0, 0.9);
    assert!((g - 0.9).abs() < 1e-6);
    // A band one side holds alone is not turned down, however loud.
    let g = keep_power(
        &[1.0, 0.0],
        &[0.0, 1.0],
        &[10.0, 0.1],
        &[0.01, 1.0],
        1.0,
        0.25,
    );
    assert!((g - 1.0).abs() < 1e-6, "{g}");
}

#[test]
fn loops_start_back_from_the_end() {
    let segs = loop_segments(100.0, 0.5, 2.0, &[2.0, 1.0, 0.5]);
    assert_eq!(segs, vec![(99.0, 100.0), (99.0, 99.5), (99.0, 99.25)]);
}

/// Levels of `n` beats at -8 dB, with `dip` beats set lower and the beats
/// from `end` at -23 dB.
fn levels(n: usize, dips: &[(usize, f64)], end: usize) -> Vec<f64> {
    let mut l = vec![-8.0; n];
    for (k, v) in dips {
        l[*k] = *v;
    }
    for v in l.iter_mut().skip(end) {
        *v = -23.0;
    }
    l
}

#[test]
fn the_exit_is_the_end_of_the_music_and_the_break_before_it() {
    let rules = PointRules::default();
    let l = levels(304, &[(281, -15.0), (290, -10.0)], 300);
    let e = exits(&l, &rules, 16);
    assert_eq!(e[0].end, 300);
    assert!((e[0].end_drop_db - 15.0).abs() < 1e-9);
    assert_eq!((e[0].start, e[0].kind), (281, ExitKind::Dip));
    assert!((e[0].dip_db + 7.0).abs() < 1e-9);
    // The shallow one is under the 3 dB a break needs; the fallback last.
    assert_eq!(e.len(), 2);
    assert_eq!((e[1].start, e[1].kind), (284, ExitKind::Fallback));

    // Music to the last beat: the end is past the list.
    let e = exits(&levels(200, &[], 200), &rules, 16);
    assert_eq!((e[0].end, e[0].end_drop_db), (200, 0.0));
    assert_eq!(e.len(), 1);
}

#[test]
fn entries_are_the_start_and_the_phrases_past_the_intro() {
    let mut l = vec![-10.0; 300];
    for v in l.iter_mut().take(18) {
        *v = -30.0;
    }
    let e = entries(&l, &PointRules::default());
    let beats: Vec<Option<usize>> = e.iter().map(|x| x.beat).collect();
    assert_eq!(beats, vec![None, Some(32), Some(64), Some(96)]);
    assert!(e.iter().all(|x| x.intro_end == 18));
}

/// Beats every `60 / bpm` seconds from `first` for `n` beats.
fn grid(bpm: f64, first: f64, n: usize) -> Vec<f64> {
    (0..n).map(|k| first + k as f64 * 60.0 / bpm).collect()
}

/// A pair at the given tempos: the outgoing track's music stops on beat
/// 400 after a break on beat 381, the incoming one has an 18-beat intro.
fn pair(out_bpm: f64, in_bpm: f64) -> (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) {
    let ob = grid(out_bpm, 0.5, 404);
    let ol = levels(404, &[(381, -15.0)], 400);
    let ib = grid(in_bpm, 0.3, 414);
    let mut il = vec![-10.0; 414];
    for v in il.iter_mut().take(18) {
        *v = -30.0;
    }
    (ob, ol, ib, il)
}

fn tracks<'a>(p: &'a (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>)) -> (Track<'a>, Track<'a>) {
    (
        Track {
            beats: &p.0,
            levels: &p.1,
            duration: p.0.last().unwrap() + 2.0,
        },
        Track {
            beats: &p.2,
            levels: &p.3,
            duration: p.2.last().unwrap() + 2.0,
        },
    )
}

#[test]
fn the_beats_meet_on_every_stretched_candidate() {
    let p = pair(125.0, 126.0);
    let (out, inc) = tracks(&p);
    let templates = builtin();
    let plan = plan(out, inc, 12.0, &templates, &PlanRules::default());
    assert!(!plan.candidates.is_empty());
    for c in &plan.candidates {
        let t = &templates[c.template];
        let l = &c.layout;
        if matches!(t.stretch, Stretch::None { .. }) {
            continue;
        }
        // Where an incoming beat plays, in output time, while the transition
        // lasts; and where the outgoing beat it should meet plays.
        let in_out = |b: f64| match t.stretch {
            Stretch::Incoming { .. } => l.fade_start + (b - l.in_from) / l.ratio,
            _ => l.fade_start + (b - l.in_from),
        };
        // Over the part of the outgoing track the beat list covers: loops
        // run on past it.
        let until = l
            .fade_end
            .min(l.out_glide.output_time(0.0, *out.beats.last().unwrap()));
        let mut checked = 0;
        for &b in inc.beats.iter().filter(|b| **b >= l.in_from - 1e-9) {
            let at = in_out(b);
            if at > until - 1e-6 {
                break;
            }
            // The outgoing beats in output time, by its own glide.
            let nearest = out
                .beats
                .iter()
                .map(|s| l.out_glide.output_time(0.0, *s))
                .map(|o| (o - at).abs())
                .fold(f64::MAX, f64::min);
            if at >= l.fade_start {
                assert!(nearest < 0.001, "{}: {:.2} ms", t.name, nearest * 1000.0);
                checked += 1;
            }
        }
        assert!(checked >= 4, "{}: {checked} beats checked", t.name);
    }
}

#[test]
fn the_band_swap_follows_the_structure_when_there_is_one() {
    let p = pair(125.0, 126.0);
    let (out, inc) = tracks(&p);
    let templates = builtin();
    let plan = plan(out, inc, 12.0, &templates, &PlanRules::default());
    let swap = plan
        .candidates
        .iter()
        .find(|c| templates[c.template].name == "band-swap")
        .expect("a band swap");
    assert_eq!((swap.exit.start, swap.exit.end), (381, 400));
    assert_eq!(swap.layout.beats, 19);
    assert_eq!(swap.entry.beat, Some(32));
    // The outgoing track glides to the incoming tempo, and only that one.
    assert!((swap.layout.out_glide.to - 126.0 / 125.0).abs() < 1e-9);
    assert_eq!(swap.layout.in_glide, Glide::constant(1.0));
}

#[test]
fn the_same_pair_gets_the_same_plan() {
    let p = pair(125.0, 126.0);
    let (out, inc) = tracks(&p);
    let templates = builtin();
    let a = plan(out, inc, 12.0, &templates, &PlanRules::default());
    let b = plan(out, inc, 12.0, &templates, &PlanRules::default());
    assert_eq!(a, b);
    assert!(a.choice.is_ok());
}

#[test]
fn a_pair_too_far_apart_is_not_mixed() {
    // 12 %: over every stretch, and far too much drift unstretched.
    let p = pair(120.0, 134.4);
    let (out, inc) = tracks(&p);
    let plan = plan(out, inc, 12.0, &builtin(), &PlanRules::default());
    assert!(plan.candidates.is_empty(), "{:?}", plan.candidates.len());
    assert!(plan.choice.is_err());
    assert!(plan
        .rejected
        .iter()
        .any(|r| r.reason.starts_with("tempo gap")));
}

#[test]
fn an_unsteady_grid_is_not_mixed() {
    let mut p = pair(125.0, 126.0);
    // A quarter of a beat of scatter on the outgoing beats.
    for (k, b) in p.0.iter_mut().enumerate() {
        *b += if k % 2 == 0 { 0.12 } else { -0.12 };
    }
    let (out, inc) = tracks(&p);
    let plan = plan(out, inc, 12.0, &builtin(), &PlanRules::default());
    assert!(plan.choice.is_err());
    assert!(plan.rejected.iter().all(|r| r.reason.contains("unsteady")
        || r.reason.contains("too")
        || r.reason.contains("break")));
}

#[test]
fn under_the_threshold_the_pair_is_not_mixed() {
    let p = pair(125.0, 126.0);
    let (out, inc) = tracks(&p);
    let rules = PlanRules {
        threshold: 2.0,
        ..PlanRules::default()
    };
    let plan = plan(out, inc, 12.0, &builtin(), &rules);
    assert!(!plan.candidates.is_empty());
    let why = plan.choice.unwrap_err();
    assert!(why.contains("under"), "{why}");
}

//! Measure the tempo and energy analysis against a real music folder.
//!
//! ```text
//! cargo run --release -p waveflow-core --example analysis_bench -- <folder> [--limit N] [--out file.tsv] [--paths list.txt] [--beats dir]
//! ```
//!
//! `--beats dir` writes each file's beats to `dir/<file stem>.beats`, one
//! time in seconds per line, the layout of the Ballroom and GTZAN beat
//! annotations, so they can be scored against them. Files sharing a
//! stem overwrite each other there: fine for those datasets, whose names
//! are unique, not for a library full of `01 Intro`.
//!
//! Every audio file under `<folder>` is analysed. Files with a BPM tag
//! (`TBPM`, `BPM`, `tmpo`) are scored against it, the usual way tempo
//! estimators are judged:
//!
//! - **exact** — within 4% of the tag;
//! - **octave** — within 4% of twice, half, three times or a third of it;
//! - **wrong** — neither.
//!
//! Tags are only a reference here: WaveFlow never reads them for its own
//! tempo. They are often rounded, and now and then an octave out
//! themselves, so a few "octave" rows are the tag's fault.
//!
//! The confidence is split by outcome, which is what tells whether it
//! separates good readings from bad, and the energy parts are printed as
//! percentiles over the whole folder, which is what its midpoints are
//! set from.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lofty::file::TaggedFileExt;
use lofty::tag::ItemKey;
use waveflow_core::analysis::analyze_file;

const AUDIO: &[&str] = &[
    "mp3", "flac", "m4a", "mp4", "aac", "ogg", "oga", "opus", "wav", "aiff", "aif", "wv",
];

struct Row {
    path: PathBuf,
    tag: Option<f64>,
    bpm: Option<f64>,
    confidence: f64,
    salience: f64,
    octave_margin: f64,
    stability: f64,
    energy: Option<f64>,
    attack_db: f64,
    onset_rate: f64,
    centroid_hz: f64,
    dynamics_db: f64,
    lufs: Option<f64>,
    key: Option<String>,
    key_confidence: f64,
    chroma: Option<[f64; 12]>,
}

#[derive(PartialEq, Clone, Copy)]
enum Outcome {
    Exact,
    Octave,
    Wrong,
}

fn outcome(bpm: f64, tag: f64) -> Outcome {
    let close = |b: f64| (b / tag - 1.0).abs() <= 0.04;
    if close(bpm) {
        Outcome::Exact
    } else if [2.0, 0.5, 3.0, 1.0 / 3.0].iter().any(|r| close(bpm * r)) {
        Outcome::Octave
    } else {
        Outcome::Wrong
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) != Some(".waveflow") {
                collect(&path, out);
            }
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| AUDIO.contains(&e.to_ascii_lowercase().as_str()))
        {
            out.push(path);
        }
    }
}

/// A path as `--paths` compares it: `/` separators and lower case, so a
/// list written on one system matches the walk on another.
fn path_key(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

fn tag_bpm(path: &Path) -> Option<f64> {
    let tagged = lofty::read_from_path(path).ok()?;
    for tag in tagged.tags() {
        for key in [ItemKey::Bpm, ItemKey::IntegerBpm] {
            if let Some(value) = tag.get_string(key) {
                if let Ok(bpm) = value.trim().replace(',', ".").parse::<f64>() {
                    if bpm > 20.0 && bpm < 400.0 {
                        return Some(bpm);
                    }
                }
            }
        }
    }
    None
}

fn percentiles(mut values: Vec<f64>) -> String {
    if values.is_empty() {
        return "-".into();
    }
    values.sort_by(f64::total_cmp);
    let at = |p: f64| values[((values.len() - 1) as f64 * p).round() as usize];
    format!(
        "p10 {:.2}  p25 {:.2}  p50 {:.2}  p75 {:.2}  p90 {:.2}",
        at(0.1),
        at(0.25),
        at(0.5),
        at(0.75),
        at(0.9)
    )
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(folder) = args.next() else {
        eprintln!("usage: analysis_bench <folder> [--limit N] [--out file.tsv]");
        std::process::exit(2);
    };
    let mut limit = usize::MAX;
    let mut out_path: Option<String> = None;
    let mut only: Option<std::collections::HashSet<String>> = None;
    let mut beats_dir: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--limit" => limit = args.next().and_then(|v| v.parse().ok()).unwrap_or(limit),
            "--out" => out_path = args.next(),
            // One path per line: analyse only those (case-insensitive).
            "--paths" => {
                let list = std::fs::read_to_string(args.next().unwrap_or_default())
                    .expect("read --paths file");
                only = Some(list.lines().map(|l| path_key(l.trim())).collect());
            }
            "--beats" => {
                let Some(dir) = args.next().map(PathBuf::from) else {
                    eprintln!("--beats needs a directory");
                    std::process::exit(2);
                };
                std::fs::create_dir_all(&dir).expect("create --beats dir");
                beats_dir = Some(dir);
            }
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }

    let mut files = Vec::new();
    collect(Path::new(&folder), &mut files);
    if let Some(only) = &only {
        files.retain(|p| only.contains(&path_key(&p.display().to_string())));
    }
    files.sort();
    files.truncate(limit);
    let total = files.len();
    eprintln!("{total} files");

    let queue = Arc::new(Mutex::new(files.into_iter().collect::<VecDeque<_>>()));
    let rows = Arc::new(Mutex::new(Vec::<Row>::new()));
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    let handles: Vec<_> = (0..workers)
        .map(|_| {
            let queue = Arc::clone(&queue);
            let rows = Arc::clone(&rows);
            let beats_dir = beats_dir.clone();
            std::thread::spawn(move || loop {
                let Some(path) = queue.lock().unwrap().pop_front() else {
                    break;
                };
                let tag = tag_bpm(&path);
                let Ok(result) = analyze_file(&path) else {
                    eprintln!("skipped {}", path.display());
                    continue;
                };
                if let (Some(dir), Some(stem)) = (&beats_dir, path.file_stem()) {
                    let lines: String = result
                        .rhythm
                        .beats
                        .iter()
                        .map(|b| {
                            format!(
                                "{b:.4}
"
                            )
                        })
                        .collect();
                    let out = dir.join(format!("{}.beats", stem.to_string_lossy()));
                    // Reported, not fatal: a panic here would end this
                    // worker and quietly drop its share of the queue.
                    if let Err(e) = std::fs::write(&out, lines) {
                        eprintln!("could not write {}: {e}", out.display());
                    }
                }
                let t = result.rhythm.tempo;
                let e = result.rhythm.energy;
                let row = Row {
                    path,
                    tag,
                    bpm: result.bpm,
                    confidence: t.map_or(0.0, |t| t.confidence),
                    salience: t.map_or(0.0, |t| t.salience),
                    octave_margin: t.map_or(0.0, |t| t.octave_margin),
                    stability: t.map_or(0.0, |t| t.stability),
                    energy: result.energy,
                    attack_db: e.map_or(0.0, |e| e.attack_db),
                    onset_rate: e.map_or(0.0, |e| e.onset_rate),
                    centroid_hz: e.map_or(0.0, |e| e.centroid_hz),
                    dynamics_db: e.map_or(0.0, |e| e.dynamics_db),
                    lufs: result.loudness_lufs,
                    key: result.key.map(|k| k.name().to_string()),
                    key_confidence: result.key.map_or(0.0, |k| k.confidence),
                    chroma: result.chroma,
                };
                let mut rows = rows.lock().unwrap();
                rows.push(row);
                if rows.len() % 50 == 0 {
                    eprintln!("{}/{total}", rows.len());
                }
            })
        })
        .collect();
    for handle in handles {
        let _ = handle.join();
    }
    let mut rows = Arc::try_unwrap(rows).ok().unwrap().into_inner().unwrap();
    rows.sort_by(|a, b| a.path.cmp(&b.path));

    if let Some(out_path) = out_path {
        let mut f = File::create(&out_path).expect("create output");
        writeln!(
            f,
            "path\ttag\tbpm\toutcome\tconfidence\tsalience\toctave_margin\tstability\tenergy\tattack_db\tonset_rate\tcentroid_hz\tdynamics_db\tlufs\tkey\tkey_confidence\tchroma"
        )
        .unwrap();
        for r in &rows {
            let o = match (r.bpm, r.tag) {
                (Some(b), Some(t)) => match outcome(b, t) {
                    Outcome::Exact => "exact",
                    Outcome::Octave => "octave",
                    Outcome::Wrong => "wrong",
                },
                _ => "",
            };
            let opt = |v: Option<f64>| v.map_or(String::new(), |v| format!("{v:.2}"));
            writeln!(
                f,
                "{}\t{}\t{}\t{o}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{}\t{:.3}\t{:.3}\t{:.0}\t{:.2}\t{}\t{}\t{:.3}\t{}",
                r.path.display(),
                opt(r.tag),
                opt(r.bpm),
                r.confidence,
                r.salience,
                r.octave_margin,
                r.stability,
                opt(r.energy),
                r.attack_db,
                r.onset_rate,
                r.centroid_hz,
                r.dynamics_db,
                opt(r.lufs),
                r.key.as_deref().unwrap_or(""),
                r.key_confidence,
                r.chroma.map_or(String::new(), |c| c
                    .iter()
                    .map(|v| format!("{v:.5}"))
                    .collect::<Vec<_>>()
                    .join(",")),
            )
            .unwrap();
        }
        eprintln!("wrote {out_path}");
    }

    let scored: Vec<(Outcome, &Row)> = rows
        .iter()
        .filter_map(|r| Some((outcome(r.bpm?, r.tag?), r)))
        .collect();
    let count = |o: Outcome| scored.iter().filter(|(x, _)| *x == o).count();
    let n = scored.len().max(1) as f64;
    println!(
        "analysed {} files, {} with a BPM tag",
        rows.len(),
        scored.len()
    );
    println!(
        "exact {:.1}%   octave {:.1}%   wrong {:.1}%",
        100.0 * count(Outcome::Exact) as f64 / n,
        100.0 * count(Outcome::Octave) as f64 / n,
        100.0 * count(Outcome::Wrong) as f64 / n
    );
    for (label, o) in [
        ("exact", Outcome::Exact),
        ("octave", Outcome::Octave),
        ("wrong", Outcome::Wrong),
    ] {
        let conf: Vec<f64> = scored
            .iter()
            .filter(|(x, _)| *x == o)
            .map(|(_, r)| r.confidence)
            .collect();
        println!("confidence | {label:6} {}", percentiles(conf));
    }
    // Accuracy among readings above a few confidence cut-offs: what a
    // consumer trusting only those would get.
    for cut in [0.2, 0.4, 0.6] {
        let kept: Vec<&(Outcome, &Row)> =
            scored.iter().filter(|(_, r)| r.confidence >= cut).collect();
        let exact = kept.iter().filter(|(o, _)| *o == Outcome::Exact).count();
        println!(
            "confidence >= {cut}: keeps {:.0}% of tagged, {:.1}% of those exact",
            100.0 * kept.len() as f64 / n,
            100.0 * exact as f64 / kept.len().max(1) as f64
        );
    }
    let all = |f: fn(&Row) -> f64| rows.iter().filter(|r| r.energy.is_some()).map(f).collect();
    println!(
        "energy      {}",
        percentiles(all(|r| r.energy.unwrap_or(0.0)))
    );
    println!("attack_db   {}", percentiles(all(|r| r.attack_db)));
    println!("onset_rate  {}", percentiles(all(|r| r.onset_rate)));
    println!("centroid_hz {}", percentiles(all(|r| r.centroid_hz)));
    println!("dynamics_db {}", percentiles(all(|r| r.dynamics_db)));
}

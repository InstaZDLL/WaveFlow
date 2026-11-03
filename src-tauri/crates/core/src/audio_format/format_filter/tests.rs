use std::path::{Path, PathBuf};
use std::str::FromStr;

use lofty::file::{AudioFile, FileType, TaggedFileExt};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use super::formats_condition;
use crate::scanner::extract::{probe_playable_stream, read_tagged_file};
use crate::scanner::file_type_label;
use crate::smart_playlists::custom::{run_query, CustomRules, Predicate, RuleNode};

/// Real files, one per stream the filters have to tell apart. Each is 0.1 s
/// of sine from ffmpeg; `stream.wav` was written to a pipe, so its `data`
/// chunk declares no length and only the decoder can describe it.
const FIXTURES: &[&str] = &[
    "vorbis.ogg",
    "opus.opus",
    "opus_in.ogg",
    "flac_in.ogg",
    "alac.m4a",
    "aac.m4a",
    "stream.wav",
];

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/formats")
        .join(name)
}

/// What the scanner stores for a file: the same two readers in the same
/// order as `extract_file`, so a label the scanner changes reaches these
/// tests instead of leaving them matching a spelling nobody writes.
fn scanned(path: &Path) -> (Option<String>, Option<i64>) {
    match read_tagged_file(path) {
        Ok(tagged) => (
            file_type_label(tagged.file_type()),
            tagged
                .properties()
                .bit_depth()
                .map(i64::from)
                .filter(|depth| *depth > 0),
        ),
        Err(_) => {
            let stream = probe_playable_stream(path).expect("the decoder describes it");
            (stream.codec, stream.bit_depth)
        }
    }
}

/// The real profile schema, foreign keys on, holding the fixtures as the
/// scanner labels them. The rows no fixture can stand for carry the label
/// their own code path writes: the DSD reader lives in the app crate, and
/// the sandbox measured `DSD64` on both containers.
async fn library() -> SqlitePool {
    let opts = SqliteConnectOptions::from_str(":memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'L', 0, 0)")
        .execute(&pool)
        .await
        .unwrap();

    let mut rows: Vec<(String, Option<String>, Option<i64>)> = FIXTURES
        .iter()
        .map(|name| {
            let (codec, depth) = scanned(&fixture(name));
            (name.to_string(), codec, depth)
        })
        .collect();
    for (name, codec, depth) in [
        ("dsd.dsf", Some("DSD64".to_string()), Some(1)),
        ("dsd.DFF", Some("DSD64".to_string()), Some(1)),
        ("plain.flac", file_type_label(FileType::Flac), Some(16)),
        ("plain.wav", file_type_label(FileType::Wav), Some(16)),
        ("plain.aiff", file_type_label(FileType::Aiff), Some(16)),
        ("described.aif", Some("PCM".to_string()), Some(16)),
        ("plain.mp3", file_type_label(FileType::Mpeg), None),
        ("plain.wv", file_type_label(FileType::WavPack), Some(16)),
        ("plain.ape", file_type_label(FileType::Ape), Some(16)),
        ("unknown.ogg", None, None),
        ("blank.ogg", Some(String::new()), None),
    ] {
        rows.push((name.to_string(), codec, depth));
    }
    for (name, codec, depth) in rows {
        sqlx::query(
            "INSERT INTO track
                (library_id, file_path, file_hash, file_size, file_modified,
                 title, duration_ms, added_at, codec, bit_depth)
             VALUES (1, ?, 'h', 0, 0, 'T', 1000, 0, ?, ?)",
        )
        .bind(format!("/music/{name}"))
        .bind(codec)
        .bind(depth)
        .execute(&pool)
        .await
        .unwrap();
    }
    pool
}

fn names(mut paths: Vec<String>) -> Vec<String> {
    for path in &mut paths {
        *path = path.rsplit('/').next().unwrap().to_string();
    }
    paths.sort();
    paths
}

/// What the search panel gets for these chips.
async fn chips(pool: &SqlitePool, formats: &[&str]) -> Vec<String> {
    let (condition, values) = formats_condition(formats);
    let sql = format!("SELECT t.file_path FROM track t WHERE {condition}");
    let mut query = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(sql));
    for value in values {
        query = query.bind(value);
    }
    names(query.fetch_all(pool).await.unwrap())
}

/// What a smart-playlist rule gets for this tree.
async fn rule(pool: &SqlitePool, tree: RuleNode) -> Vec<String> {
    let ids = run_query(
        pool,
        &CustomRules {
            tree,
            sort: None,
            limit: None,
        },
    )
    .await
    .unwrap();
    let mut paths = Vec::with_capacity(ids.len());
    for id in ids {
        let path: String = sqlx::query_scalar("SELECT file_path FROM track WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
        paths.push(path);
    }
    names(paths)
}

fn format_leaf(value: &str) -> RuleNode {
    RuleNode::Leaf {
        predicate: Predicate::Format {
            value: value.to_string(),
        },
    }
}

/// The bug: the database says `Vorbis`, the chip and the rule said `ogg`,
/// and neither ever matched a file. Asserted through the label the scanner
/// gives a real file, so it is the stored spelling that is under test.
#[tokio::test]
async fn ogg_finds_vorbis_and_leaves_opus_to_its_own_chip() {
    let pool = library().await;
    assert_eq!(scanned(&fixture("vorbis.ogg")).0.as_deref(), Some("Vorbis"));
    assert_eq!(scanned(&fixture("opus_in.ogg")).0.as_deref(), Some("Opus"));

    assert_eq!(chips(&pool, &["OGG"]).await, ["vorbis.ogg"]);
    assert_eq!(rule(&pool, format_leaf("ogg")).await, ["vorbis.ogg"]);
    assert_eq!(chips(&pool, &["OPUS"]).await, ["opus.opus", "opus_in.ogg"]);
    assert_eq!(
        rule(&pool, format_leaf("opus")).await,
        ["opus.opus", "opus_in.ogg"]
    );
    assert_eq!(
        chips(&pool, &["OGG", "OPUS"]).await,
        ["opus.opus", "opus_in.ogg", "vorbis.ogg"]
    );
}

/// Every name the two filters offer finds its files and only those. The
/// WavPack and Monkey's Audio labels go through the plain comparison, as any
/// codec added after this list will.
#[tokio::test]
async fn every_offered_format_finds_its_files() {
    let pool = library().await;
    let expected: &[(&str, &[&str])] = &[
        ("FLAC", &["flac_in.ogg", "plain.flac"]),
        ("WAV", &["plain.wav", "stream.wav"]),
        ("AIFF", &["described.aif", "plain.aiff"]),
        ("ALAC", &["alac.m4a"]),
        ("DSF", &["dsd.dsf"]),
        ("DFF", &["dsd.DFF"]),
        ("MP3", &["plain.mp3"]),
        ("AAC", &["aac.m4a"]),
        ("WavPack", &["plain.wv"]),
        ("APE", &["plain.ape"]),
    ];
    for (format, files) in expected {
        assert_eq!(chips(&pool, &[format]).await, *files, "chip {format}");
        assert_eq!(
            rule(&pool, format_leaf(&format.to_lowercase())).await,
            *files,
            "rule {format}"
        );
    }
}

/// `file_type_label` calls every MP4 `AAC`, so the two chips rest on the bit
/// depth, which lofty reports for ALAC and not for AAC.
#[test]
fn the_bit_depth_tells_alac_from_aac() {
    assert_eq!(
        scanned(&fixture("alac.m4a")),
        (Some("AAC".to_string()), Some(16))
    );
    assert_eq!(
        scanned(&fixture("aac.m4a")),
        (Some("AAC".to_string()), None)
    );
}

/// A track of unknown codec matches no format, not even an empty one, and so
/// belongs to every "not this format". `NOT (NULL)` would have lost it from
/// both sides.
#[tokio::test]
async fn a_track_of_unknown_codec_is_in_every_negation() {
    let pool = library().await;
    for format in ["ogg", "aac", "alac", "dsf", "wav", "aiff", "flac", ""] {
        assert!(
            !chips(&pool, &[format])
                .await
                .contains(&"unknown.ogg".to_string()),
            "{format}"
        );
        let negated = RuleNode::Not {
            child: Box::new(format_leaf(format)),
        };
        assert!(
            rule(&pool, negated)
                .await
                .contains(&"unknown.ogg".to_string()),
            "not {format}"
        );
    }
}

#[tokio::test]
async fn no_format_matches_nothing() {
    let pool = library().await;
    assert!(chips(&pool, &[]).await.is_empty());
    // Not even a row whose codec is an empty string.
    assert!(chips(&pool, &[""]).await.is_empty());
    assert!(rule(&pool, format_leaf("")).await.is_empty());
}

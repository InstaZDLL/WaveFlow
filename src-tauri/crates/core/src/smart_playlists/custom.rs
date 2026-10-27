//! User-defined smart playlists with a **recursive boolean rule tree**.
//!
//! The editor builds a `RuleNode` tree (`All` / `Any` / `Not` / `Leaf`)
//! and persists it as JSON inside `playlist.smart_rules`. The
//! materializer walks the tree to emit a single SQL `WHERE` clause —
//! every join-needing predicate goes through an `EXISTS` subquery so
//! the tree can nest arbitrarily without DISTINCT or Cartesian
//! explosions.
//!
//! ## Schema versions
//!
//! - **v1** (pre-tree): flat optional predicates (`title_contains`,
//!   `year_min`, `genre_ids: Vec<i64>`, …) all AND-combined.
//! - **v2** (current): explicit `tree: RuleNode`.
//!
//! The deserializer auto-migrates v1 payloads to v2 at load time so
//! existing user playlists keep working without a DB migration. v1
//! gets folded into an `All` at the root with each multi-value field
//! wrapped in `Any` (matches the previous OR-within-AND semantics).
//!
//! ## Refresh strategy
//!
//! Re-evaluated on demand (`regenerate_custom_smart_playlist`) and
//! once at app startup so tracks imported since the last run are
//! picked up. Live re-materialize on every library write would be
//! wasteful (a 10k import would burn it once per file); the
//! on-startup pass is the practical compromise.

use serde::{Deserialize, Deserializer, Serialize};
// `SqlitePool` is only used by the materialiser functions below
// (`materialize`, `run_query`) — both gated on `feature = "sqlite"`.
// The rule-tree types in this module are storage-agnostic and stay
// available in postgres-only builds.
#[cfg(feature = "sqlite")]
use sqlx::SqlitePool;

#[cfg(feature = "sqlite")]
use crate::error::{CoreError, CoreResult};

// =============================================================================
// Rule tree types
// =============================================================================

/// One node in the rule tree. `All` / `Any` / `Not` are group ops;
/// `Leaf` carries the actual predicate. JSON shape uses an internal
/// `type` tag:
///
/// ```json
/// {"type":"all","children":[
///   {"type":"any","children":[
///     {"type":"leaf","predicate":{"kind":"artist_contains","value":"Daft Punk"}},
///     {"type":"leaf","predicate":{"kind":"artist_contains","value":"Justice"}}
///   ]},
///   {"type":"not","child":{"type":"leaf","predicate":{"kind":"liked"}}}
/// ]}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuleNode {
    /// Every child must match (logical AND). An empty `All` matches
    /// every available track — used as the canonical "no filter" root
    /// so the editor never has to special-case `null`.
    All { children: Vec<RuleNode> },
    /// At least one child must match (logical OR). An empty `Any`
    /// matches nothing — degenerate but well-defined.
    Any { children: Vec<RuleNode> },
    /// Negation. A single child so the editor can't accidentally
    /// build `NOT (A, B)` and confuse users about what's being negated.
    Not { child: Box<RuleNode> },
    /// A single comparable predicate.
    Leaf { predicate: Predicate },
}

impl Default for RuleNode {
    fn default() -> Self {
        RuleNode::All { children: vec![] }
    }
}

/// Atomic predicate evaluated against a single `track` row. Unit
/// variants (`HiRes`, `Liked`) serialize as `{"kind":"hi_res"}` — no
/// `value` field needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Predicate {
    TitleContains {
        value: String,
    },
    ArtistContains {
        value: String,
    },
    AlbumContains {
        value: String,
    },
    /// Single genre. Multi-genre selection is expressed via `Any` of
    /// these so the tree shape is consistent across all multi-value
    /// editors.
    GenreIs {
        value: i64,
    },
    YearMin {
        value: i64,
    },
    YearMax {
        value: i64,
    },
    BpmMin {
        value: f64,
    },
    BpmMax {
        value: f64,
    },
    DurationMinMs {
        value: i64,
    },
    DurationMaxMs {
        value: i64,
    },
    /// Single file extension (lowercase, no dot). Multi-format
    /// selection uses `Any` of these.
    Format {
        value: String,
    },
    /// Hi-Res = sample rate ≥ 88.2 kHz OR bit depth ≥ 24.
    HiRes,
    /// A lossless file made from a lossy one: its highs stop at an
    /// encoder's wall (`analysis::bandwidth`).
    FakeLossless,
    Liked,
    /// Minimum POPM rating (0-255). Editor stores `Math.round(stars / 5 * 255)`.
    RatingMin {
        value: i64,
    },
    /// At least this many plays, counted the way the rest of the app
    /// counts them: one `play_event` row is one play, with no minimum
    /// listened time. Statistics and Wrapped both answer `COUNT(*)`, so
    /// a rule saying "played at least 20 times" has to agree with the
    /// number the user just read on the statistics page.
    PlayCountMin {
        value: i64,
    },
    /// At most this many plays. `0` is the useful value — it is how a
    /// rule says *never played*, which no combination of the other
    /// predicates expresses.
    PlayCountMax {
        value: i64,
    },
    /// Played at least once within the last N days.
    ///
    /// Relative rather than absolute on purpose: a rule set is stored
    /// once and re-evaluated for years. A rule pinned to a date drifts
    /// into meaning something its author never wrote — "since March"
    /// ages into "in the last four years" — while a window keeps saying
    /// what it said the day it was built. "Not played since" is this
    /// under a `Not`, which also takes in the tracks that were never
    /// played at all, as it should.
    PlayedInLastDays {
        value: i64,
    },
    /// Added to the library within the last N days. Relative for the
    /// same reason as [`Predicate::PlayedInLastDays`].
    AddedInLastDays {
        value: i64,
    },
    /// Sample rate in Hz, at least this.
    ///
    /// [`Predicate::HiRes`] is the OR of two fixed thresholds and
    /// cannot say "88.2 kHz but 16-bit"; these two say it separately.
    SampleRateMin {
        value: i64,
    },
    /// Bit depth, at least this. Lossy codecs carry none, so a file
    /// with no depth recorded matches no bound in either direction —
    /// see the note on NULLs in `build_predicate_sql`.
    BitDepthMin {
        value: i64,
    },
    /// A specific disc of a multi-disc set.
    DiscNumberIs {
        value: i64,
    },
    /// The file's path contains this fragment — the "everything under
    /// this folder" rule, which is otherwise unexpressible.
    ///
    /// Both sides are compared with `/` separators so a rule written on
    /// one platform reads the same on another, and so the user can type
    /// whichever slash their keyboard offers.
    PathContains {
        value: String,
    },
    /// The file carries this tag at all, whatever its value — a rip
    /// source, a catalogue number, a mood written by the user's own
    /// tagger. The keys come from `track_tag` (#588), so the editor can
    /// offer the ones that are actually in the library.
    TagPresent {
        key: String,
    },
    /// The file carries this tag and its value contains this fragment.
    TagContains {
        key: String,
        value: String,
    },
}

// =============================================================================
// Sort / limit / outer rules
// =============================================================================

/// Sort order applied before truncation. SQL fragments are hand-rolled
/// because dynamic ORDER BY through binds isn't allowed in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum CustomSort {
    #[default]
    AddedDesc,
    AddedAsc,
    YearDesc,
    YearAsc,
    TitleAsc,
    ArtistAsc,
    Random,
}

#[cfg(feature = "sqlite")]
fn order_by_sql(sort: &CustomSort) -> &'static str {
    match sort {
        CustomSort::AddedDesc => "t.added_at DESC",
        CustomSort::AddedAsc => "t.added_at ASC",
        CustomSort::YearDesc => "COALESCE(t.year, 0) DESC, t.title ASC",
        CustomSort::YearAsc => "COALESCE(t.year, 9999) ASC, t.title ASC",
        CustomSort::TitleAsc => "t.title ASC",
        CustomSort::ArtistAsc => {
            "COALESCE((SELECT name FROM artist WHERE id = t.primary_artist), '') ASC, \
             t.title ASC"
        }
        CustomSort::Random => "RANDOM()",
    }
}

/// Editor-facing rule set: the tree + the outer sort/limit fields.
/// `Default` returns an empty tree → matches every available track
/// (the editor relies on this for the "blank slate" state).
#[derive(Debug, Clone, Default, Serialize)]
pub struct CustomRules {
    pub tree: RuleNode,
    pub sort: Option<CustomSort>,
    pub limit: Option<i64>,
}

#[cfg(feature = "sqlite")]
const HARD_LIMIT: i64 = 5_000;

// =============================================================================
// v1 → v2 deserialize migration
// =============================================================================

/// On-disk shape used by `Deserialize`. We accept both the v1 flat
/// schema (legacy fields at the top) and the v2 tree schema; if `tree`
/// is missing we migrate the legacy fields into an `All` root with
/// multi-value selectors wrapped in `Any`.
#[derive(Deserialize)]
struct RawCustomRules {
    #[serde(default)]
    tree: Option<RuleNode>,
    #[serde(default)]
    sort: Option<CustomSort>,
    #[serde(default)]
    limit: Option<i64>,
    // ---- legacy flat fields ----
    #[serde(default)]
    title_contains: Option<String>,
    #[serde(default)]
    artist_contains: Option<String>,
    #[serde(default)]
    album_contains: Option<String>,
    #[serde(default)]
    genre_ids: Option<Vec<i64>>,
    #[serde(default)]
    year_min: Option<i64>,
    #[serde(default)]
    year_max: Option<i64>,
    #[serde(default)]
    bpm_min: Option<f64>,
    #[serde(default)]
    bpm_max: Option<f64>,
    #[serde(default)]
    duration_min_ms: Option<i64>,
    #[serde(default)]
    duration_max_ms: Option<i64>,
    #[serde(default)]
    formats: Option<Vec<String>>,
    #[serde(default)]
    hi_res_only: Option<bool>,
    #[serde(default)]
    liked_only: Option<bool>,
    #[serde(default)]
    rating_min: Option<i64>,
}

impl<'de> Deserialize<'de> for CustomRules {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawCustomRules::deserialize(deserializer)?;
        let tree = raw.tree.clone().unwrap_or_else(|| migrate_legacy(&raw));
        Ok(CustomRules {
            tree,
            sort: raw.sort,
            limit: raw.limit,
        })
    }
}

fn migrate_legacy(raw: &RawCustomRules) -> RuleNode {
    let mut children: Vec<RuleNode> = Vec::new();
    let leaf = |p: Predicate| RuleNode::Leaf { predicate: p };

    if let Some(v) = raw
        .title_contains
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        children.push(leaf(Predicate::TitleContains {
            value: v.to_string(),
        }));
    }
    if let Some(v) = raw
        .artist_contains
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        children.push(leaf(Predicate::ArtistContains {
            value: v.to_string(),
        }));
    }
    if let Some(v) = raw
        .album_contains
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        children.push(leaf(Predicate::AlbumContains {
            value: v.to_string(),
        }));
    }
    if let Some(ids) = raw.genre_ids.as_ref().filter(|v| !v.is_empty()) {
        let any_children: Vec<RuleNode> = ids
            .iter()
            .map(|id| leaf(Predicate::GenreIs { value: *id }))
            .collect();
        children.push(RuleNode::Any {
            children: any_children,
        });
    }
    if let Some(v) = raw.year_min {
        children.push(leaf(Predicate::YearMin { value: v }));
    }
    if let Some(v) = raw.year_max {
        children.push(leaf(Predicate::YearMax { value: v }));
    }
    if let Some(v) = raw.bpm_min {
        children.push(leaf(Predicate::BpmMin { value: v }));
    }
    if let Some(v) = raw.bpm_max {
        children.push(leaf(Predicate::BpmMax { value: v }));
    }
    if let Some(v) = raw.duration_min_ms {
        children.push(leaf(Predicate::DurationMinMs { value: v }));
    }
    if let Some(v) = raw.duration_max_ms {
        children.push(leaf(Predicate::DurationMaxMs { value: v }));
    }
    if let Some(formats) = raw.formats.as_ref().filter(|v| !v.is_empty()) {
        let any_children: Vec<RuleNode> = formats
            .iter()
            .map(|f| {
                leaf(Predicate::Format {
                    value: f.to_lowercase(),
                })
            })
            .collect();
        children.push(RuleNode::Any {
            children: any_children,
        });
    }
    if raw.hi_res_only == Some(true) {
        children.push(leaf(Predicate::HiRes));
    }
    if raw.liked_only == Some(true) {
        children.push(leaf(Predicate::Liked));
    }
    if let Some(v) = raw.rating_min.filter(|r| *r > 0) {
        children.push(leaf(Predicate::RatingMin { value: v }));
    }

    RuleNode::All { children }
}

// =============================================================================
// SQL builder (sqlite-only)
// =============================================================================

#[cfg(feature = "sqlite")]
enum BindValue {
    Int(i64),
    Real(f64),
    Text(String),
}

/// Walk the tree and emit a `(where_sql, binds)` pair. Group ops
/// short-circuit on empty children to keep the SQL tidy:
///
/// - empty `All` → `"1=1"` (matches all rows)
/// - empty `Any` → `"0=1"` (matches nothing)
#[cfg(feature = "sqlite")]
fn build_node_sql(node: &RuleNode, binds: &mut Vec<BindValue>, now_ms: i64) -> String {
    match node {
        RuleNode::All { children } => {
            if children.is_empty() {
                return "1=1".to_string();
            }
            let parts: Vec<String> = children
                .iter()
                .map(|c| build_node_sql(c, binds, now_ms))
                .collect();
            format!("({})", parts.join(" AND "))
        }
        RuleNode::Any { children } => {
            if children.is_empty() {
                return "0=1".to_string();
            }
            let parts: Vec<String> = children
                .iter()
                .map(|c| build_node_sql(c, binds, now_ms))
                .collect();
            format!("({})", parts.join(" OR "))
        }
        RuleNode::Not { child } => {
            let inner = build_node_sql(child, binds, now_ms);
            format!("NOT ({inner})")
        }
        RuleNode::Leaf { predicate } => build_predicate_sql(predicate, binds, now_ms),
    }
}

/// The escape character for every `LIKE` pattern the builder emits.
///
/// `!` rather than the usual backslash because the one pattern most
/// likely to contain a backslash is a path, and a rule reading
/// `LIKE ? ESCAPE '\\'` over a Windows path is a trap waiting for
/// whoever touches the separator folding next.
#[cfg(feature = "sqlite")]
const LIKE_ESCAPE: char = '!';

/// A user's text as a `LIKE` pattern: metacharacters made literal, then
/// wrapped in wildcards.
///
/// Without this, `%` and `_` typed into a "contains" field are
/// wildcards — and `_` is not an exotic character in a filename or a
/// catalogue number. A rule for `My_Music` would quietly also take in
/// `MyXMusic`, which nobody asked for and nothing on screen would
/// explain. The escape character is escaped first, or escaping the
/// others would corrupt any literal `!` the user typed.
#[cfg(feature = "sqlite")]
fn like_contains(value: &str) -> String {
    let escaped = value
        .trim()
        .replace(LIKE_ESCAPE, &format!("{LIKE_ESCAPE}{LIKE_ESCAPE}"))
        .replace('%', &format!("{LIKE_ESCAPE}%"))
        .replace('_', &format!("{LIKE_ESCAPE}_"));
    format!("%{escaped}%")
}

/// The `LIKE` operator with its escape clause, for a column expression.
///
/// Kept in one place so the two halves can never drift apart: a
/// pattern escaped without the clause matches the escape character
/// literally, which is worse than not escaping at all.
///
/// No `COLLATE NOCASE`, which these fragments used to carry. SQLite's
/// `LIKE` **ignores collating sequences** and is already
/// case-insensitive across ASCII, so the clause never decided
/// anything — and once `ESCAPE` follows the pattern, a trailing
/// `COLLATE` binds to the escape character rather than to the
/// comparison, which is a claim about the SQL that is not true.
#[cfg(feature = "sqlite")]
fn like_clause(column: &str) -> String {
    format!("{column} LIKE ? ESCAPE '{LIKE_ESCAPE}'")
}

/// Epoch milliseconds `days` before `now_ms`, the cut-off of a relative
/// window. Saturating so an absurd day count clamps instead of wrapping
/// into the future, which would turn "in the last N days" into a rule
/// matching nothing — the opposite of what a very large N asks for.
#[cfg(feature = "sqlite")]
fn days_ago_ms(now_ms: i64, days: i64) -> i64 {
    now_ms.saturating_sub(days.max(0).saturating_mul(86_400_000))
}

/// One SQL fragment per predicate. Every join-needing predicate uses
/// an `EXISTS` subquery so the tree can be nested arbitrarily without
/// row duplication at the top level.
///
/// # NULL columns and `Not`
///
/// Bounds on a nullable column are written `(col IS NOT NULL AND col >=
/// ?)` rather than left to SQLite's three-valued logic. Without the
/// guard the fragment evaluates to NULL for a track whose value is
/// missing, and `NOT (NULL)` is NULL too — so such a track would fall
/// out of a rule *and* out of its negation, which reads as the library
/// losing tracks. With the guard the negation takes them in, which is
/// the answer a user expects from "not hi-res" about a file whose
/// sample rate nobody recorded.
#[cfg(feature = "sqlite")]
fn build_predicate_sql(pred: &Predicate, binds: &mut Vec<BindValue>, now_ms: i64) -> String {
    match pred {
        Predicate::TitleContains { value } => {
            binds.push(BindValue::Text(like_contains(value)));
            like_clause("t.title")
        }
        Predicate::ArtistContains { value } => {
            binds.push(BindValue::Text(like_contains(value)));
            format!(
                "EXISTS (SELECT 1 FROM track_artist ta JOIN artist ar ON ar.id = ta.artist_id \
                 WHERE ta.track_id = t.id AND {})",
                like_clause("ar.name")
            )
        }
        Predicate::AlbumContains { value } => {
            binds.push(BindValue::Text(like_contains(value)));
            format!(
                "EXISTS (SELECT 1 FROM album WHERE album.id = t.album_id AND {})",
                like_clause("album.title")
            )
        }
        Predicate::GenreIs { value } => {
            binds.push(BindValue::Int(*value));
            "EXISTS (SELECT 1 FROM track_genre tg WHERE tg.track_id = t.id AND tg.genre_id = ?)"
                .to_string()
        }
        Predicate::YearMin { value } => {
            binds.push(BindValue::Int(*value));
            "(t.year IS NOT NULL AND t.year >= ?)".to_string()
        }
        Predicate::YearMax { value } => {
            binds.push(BindValue::Int(*value));
            "(t.year IS NOT NULL AND t.year <= ?)".to_string()
        }
        Predicate::BpmMin { value } => {
            binds.push(BindValue::Real(*value));
            "EXISTS (SELECT 1 FROM track_analysis ana WHERE ana.track_id = t.id \
             AND ana.bpm IS NOT NULL AND ana.bpm >= ?)"
                .to_string()
        }
        Predicate::BpmMax { value } => {
            binds.push(BindValue::Real(*value));
            "EXISTS (SELECT 1 FROM track_analysis ana WHERE ana.track_id = t.id \
             AND ana.bpm IS NOT NULL AND ana.bpm <= ?)"
                .to_string()
        }
        Predicate::DurationMinMs { value } => {
            binds.push(BindValue::Int(*value));
            "t.duration_ms >= ?".to_string()
        }
        Predicate::DurationMaxMs { value } => {
            binds.push(BindValue::Int(*value));
            "t.duration_ms <= ?".to_string()
        }
        Predicate::Format { value } => {
            binds.push(BindValue::Text(value.to_lowercase()));
            "LOWER(t.codec) = ?".to_string()
        }
        // Guarded like every other nullable bound, and for the same
        // reason: a file with neither figure recorded used to fall out
        // of this rule AND out of its negation, since `NULL OR NULL` is
        // NULL and so is `NOT (NULL)`. The positive answer is unchanged
        // — `TRUE OR NULL` was already TRUE — so only the negation
        // moves, and it moves to include the tracks nobody could see.
        Predicate::HiRes => "((t.sample_rate IS NOT NULL AND t.sample_rate >= 88200) \
             OR (t.bit_depth IS NOT NULL AND t.bit_depth >= 24))"
            .to_string(),
        Predicate::Liked => {
            "EXISTS (SELECT 1 FROM liked_track lt WHERE lt.track_id = t.id)".to_string()
        }
        Predicate::FakeLossless => concat!(
            "EXISTS (SELECT 1 FROM track_analysis ta WHERE ta.track_id = t.id AND ",
            crate::probable_fake_lossless_sql!(),
            ")"
        )
        .to_string(),
        Predicate::RatingMin { value } => {
            binds.push(BindValue::Int((*value).clamp(1, 255)));
            "(t.rating IS NOT NULL AND t.rating >= ?)".to_string()
        }
        // The count is a correlated subquery rather than an `EXISTS`
        // because the bound can be any number, `0` included. It is
        // served by `idx_play_event_track`, whose leading column is
        // `track_id`, so it reads one index range per track and never
        // the table.
        Predicate::PlayCountMin { value } => {
            binds.push(BindValue::Int(*value));
            "(SELECT COUNT(*) FROM play_event pe WHERE pe.track_id = t.id) >= ?".to_string()
        }
        Predicate::PlayCountMax { value } => {
            binds.push(BindValue::Int(*value));
            "(SELECT COUNT(*) FROM play_event pe WHERE pe.track_id = t.id) <= ?".to_string()
        }
        // `played_at` is epoch **milliseconds** (`analytics.rs` writes
        // `timestamp_millis`), so the cut-off is computed in the same
        // unit. A window only needs to know whether one play falls
        // inside it, hence `EXISTS` and not a count.
        Predicate::PlayedInLastDays { value } => {
            binds.push(BindValue::Int(days_ago_ms(now_ms, *value)));
            "EXISTS (SELECT 1 FROM play_event pe WHERE pe.track_id = t.id AND pe.played_at >= ?)"
                .to_string()
        }
        Predicate::AddedInLastDays { value } => {
            binds.push(BindValue::Int(days_ago_ms(now_ms, *value)));
            "t.added_at >= ?".to_string()
        }
        Predicate::SampleRateMin { value } => {
            binds.push(BindValue::Int(*value));
            "(t.sample_rate IS NOT NULL AND t.sample_rate >= ?)".to_string()
        }
        Predicate::BitDepthMin { value } => {
            binds.push(BindValue::Int(*value));
            "(t.bit_depth IS NOT NULL AND t.bit_depth >= ?)".to_string()
        }
        Predicate::DiscNumberIs { value } => {
            binds.push(BindValue::Int(*value));
            "(t.disc_number IS NOT NULL AND t.disc_number = ?)".to_string()
        }
        // Both sides are folded to `/` so one rule reads the same on
        // every platform: the column holds whatever separator the OS
        // that scanned the file uses, and the user types whichever one
        // their keyboard offers. SQLite does not process backslash
        // escapes inside a string literal, so `'\\'` here is one
        // backslash in the SQL.
        Predicate::PathContains { value } => {
            binds.push(BindValue::Text(like_contains(&value.replace('\\', "/"))));
            like_clause("REPLACE(t.file_path, '\\', '/')")
        }
        // `track_tag.key` is declared `COLLATE NOCASE`, so the equality
        // below is case-insensitive *and* index-backed on
        // `idx_track_tag_key` — the scanner stores keys upper-cased, and
        // a rule naming `Composer` has to find them.
        Predicate::TagPresent { key } => {
            binds.push(BindValue::Text(key.trim().to_string()));
            "EXISTS (SELECT 1 FROM track_tag tt WHERE tt.track_id = t.id AND tt.key = ?)"
                .to_string()
        }
        Predicate::TagContains { key, value } => {
            binds.push(BindValue::Text(key.trim().to_string()));
            binds.push(BindValue::Text(like_contains(value)));
            format!(
                "EXISTS (SELECT 1 FROM track_tag tt WHERE tt.track_id = t.id \
                 AND tt.key = ? AND {})",
                like_clause("tt.value")
            )
        }
    }
}

// =============================================================================
// Public materialize / query (sqlite-only)
// =============================================================================

/// Re-materialize the playlist's tracks from its rule set. Wipes
/// `playlist_track` rows for the playlist, runs the rule query, then
/// re-inserts the results in the sorted order. The rule set is read
/// from `playlist.smart_rules` so this command is idempotent (calling
/// it twice yields the same membership unless the library changed).
#[cfg(feature = "sqlite")]
pub async fn materialize(
    pool: &SqlitePool,
    playlist_id: i64,
    rules: &CustomRules,
) -> CoreResult<i64> {
    let track_ids = run_query(pool, rules).await?;

    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM playlist_track WHERE playlist_id = ?")
        .bind(playlist_id)
        .execute(&mut *tx)
        .await?;

    let now = chrono::Utc::now().timestamp_millis();
    for (idx, track_id) in track_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO playlist_track (playlist_id, track_id, position, added_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(playlist_id)
        .bind(track_id)
        .bind(idx as i64)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(playlist_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(track_ids.len() as i64)
}

/// How many tracks the rule set keeps, at a given number of matches.
///
/// Held apart from `total` because the two answer different questions
/// and the difference is the whole point of showing them: a rule
/// matching 3 000 tracks with a limit of 50 produces a playlist of 50,
/// and a counter reporting only one of those numbers misleads about the
/// other.
#[cfg(feature = "sqlite")]
fn kept_of(rules: &CustomRules, total: i64) -> i64 {
    total.min(effective_limit(rules))
}

/// The limit actually applied by [`run_query`].
#[cfg(feature = "sqlite")]
fn effective_limit(rules: &CustomRules) -> i64 {
    rules.limit.unwrap_or(HARD_LIMIT).clamp(1, HARD_LIMIT)
}

/// What the rule set matches right now, without materialising anything.
///
/// `total` is every available track the tree matches; `kept` is what
/// would end up in the playlist once the limit is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RulesCount {
    pub total: i64,
    pub kept: i64,
}

/// Count the matches without listing them — what the editor's live
/// counter asks for on every keystroke.
///
/// This is not `run_query(..).len()`: that one sorts, truncates and
/// carries up to five thousand ids back across the IPC boundary, all of
/// which a number on screen throws away. `COUNT(*)` also lets SQLite
/// stop at the index for the rules that have one, and — the part that
/// actually shows — reports the matches *above* the limit, which a
/// truncated list cannot.
#[cfg(feature = "sqlite")]
pub async fn count_matches(pool: &SqlitePool, rules: &CustomRules) -> CoreResult<RulesCount> {
    count_matches_at(pool, rules, now_ms()).await
}

/// [`count_matches`] with the clock supplied, so a test can pin the
/// relative windows instead of racing midnight.
#[cfg(feature = "sqlite")]
pub async fn count_matches_at(
    pool: &SqlitePool,
    rules: &CustomRules,
    now_ms: i64,
) -> CoreResult<RulesCount> {
    let mut binds = Vec::<BindValue>::new();
    let tree_where = build_node_sql(&rules.tree, &mut binds, now_ms);

    let mut sql = String::from("SELECT COUNT(*) FROM track t WHERE t.is_available = 1 AND ");
    sql.push_str(&tree_where);

    let total = bind_all(
        sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql)),
        binds,
    )
    .fetch_one(pool)
    .await
    .map_err(|e| CoreError::Other(format!("custom smart playlist count failed: {e}")))?;

    Ok(RulesCount {
        total,
        kept: kept_of(rules, total),
    })
}

/// Resolve the rule set into a list of track ids in the canonical sort
/// order. Public for the dry-run "Preview" button in the rule editor.
#[cfg(feature = "sqlite")]
pub async fn run_query(pool: &SqlitePool, rules: &CustomRules) -> CoreResult<Vec<i64>> {
    run_query_at(pool, rules, now_ms()).await
}

/// [`run_query`] with the clock supplied — see [`count_matches_at`].
#[cfg(feature = "sqlite")]
pub async fn run_query_at(
    pool: &SqlitePool,
    rules: &CustomRules,
    now_ms: i64,
) -> CoreResult<Vec<i64>> {
    let mut binds = Vec::<BindValue>::new();
    let tree_where = build_node_sql(&rules.tree, &mut binds, now_ms);

    let mut sql = String::from("SELECT t.id FROM track t WHERE t.is_available = 1 AND ");
    sql.push_str(&tree_where);
    sql.push_str(" ORDER BY ");
    sql.push_str(order_by_sql(
        rules.sort.as_ref().unwrap_or(&CustomSort::AddedDesc),
    ));

    sql.push_str(" LIMIT ?");
    binds.push(BindValue::Int(effective_limit(rules)));

    let rows = bind_all(
        sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql)),
        binds,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| CoreError::Other(format!("custom smart playlist query failed: {e}")))?;
    Ok(rows)
}

/// The tree emits its binds in the order its fragments appear, and the
/// `?` placeholders are positional: the two walks must stay in step, so
/// binding lives in one place rather than being repeated per query.
#[cfg(feature = "sqlite")]
fn bind_all<'q, O>(
    mut q: sqlx::query::QueryScalar<'q, sqlx::Sqlite, O, sqlx::sqlite::SqliteArguments>,
    binds: Vec<BindValue>,
) -> sqlx::query::QueryScalar<'q, sqlx::Sqlite, O, sqlx::sqlite::SqliteArguments> {
    for b in binds {
        q = match b {
            BindValue::Int(v) => q.bind(v),
            BindValue::Real(v) => q.bind(v),
            BindValue::Text(v) => q.bind(v),
        };
    }
    q
}

/// Wall clock in epoch milliseconds — the unit `play_event.played_at`
/// and `track.added_at` are both stored in.
#[cfg(feature = "sqlite")]
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests;

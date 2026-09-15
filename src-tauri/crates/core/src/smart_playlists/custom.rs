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
mod tests {
    use super::*;

    // SQL-shape assertions live in a nested module gated on `sqlite`
    // because `build_node_sql` (and its `BindValue` argument) are
    // sqlite-only. The serde / migration tests below stay outside this
    // module so a postgres-only test run still exercises the v1 → v2
    // rule-tree migration contract.
    #[cfg(feature = "sqlite")]
    mod sql_tests {
        use super::*;

        /// A pinned clock, so the relative windows below assert on a
        /// value rather than on "whatever the test machine thinks the
        /// time is". 2026-01-01T00:00:00Z.
        const NOW: i64 = 1_767_225_600_000;

        /// Helper: build SQL string from a tree (binds are discarded for
        /// readability — the tests only assert on the textual shape).
        fn sql_of(node: &RuleNode) -> String {
            let mut binds = Vec::new();
            build_node_sql(node, &mut binds, NOW)
        }

        /// Helper: the binds a tree emits, in placeholder order.
        fn binds_of(node: &RuleNode) -> Vec<BindValue> {
            let mut binds = Vec::new();
            build_node_sql(node, &mut binds, NOW);
            binds
        }

        fn leaf(predicate: Predicate) -> RuleNode {
            RuleNode::Leaf { predicate }
        }

        #[test]
        fn empty_all_matches_everything() {
            assert_eq!(sql_of(&RuleNode::All { children: vec![] }), "1=1");
        }

        #[test]
        fn empty_any_matches_nothing() {
            assert_eq!(sql_of(&RuleNode::Any { children: vec![] }), "0=1");
        }

        #[test]
        fn single_leaf_wraps_predicate() {
            let n = RuleNode::Leaf {
                predicate: Predicate::TitleContains {
                    value: "foo".into(),
                },
            };
            assert_eq!(sql_of(&n), "t.title LIKE ? ESCAPE '!'");
        }

        /// A `LIKE` metacharacter typed into a "contains" field is a
        /// character, not a wildcard — and the escape character itself
        /// has to survive being typed.
        #[test]
        fn a_contains_pattern_escapes_its_metacharacters() {
            assert_eq!(like_contains("My_Music"), "%My!_Music%");
            assert_eq!(like_contains("100%"), "%100!%%");
            assert_eq!(like_contains("Hey!"), "%Hey!!%");
        }

        #[test]
        fn and_joins_with_and() {
            let n = RuleNode::All {
                children: vec![
                    RuleNode::Leaf {
                        predicate: Predicate::HiRes,
                    },
                    RuleNode::Leaf {
                        predicate: Predicate::Liked,
                    },
                ],
            };
            let s = sql_of(&n);
            assert!(s.contains(" AND "));
            assert!(s.starts_with('(') && s.ends_with(')'));
        }

        #[test]
        fn or_joins_with_or() {
            let n = RuleNode::Any {
                children: vec![
                    RuleNode::Leaf {
                        predicate: Predicate::GenreIs { value: 1 },
                    },
                    RuleNode::Leaf {
                        predicate: Predicate::GenreIs { value: 2 },
                    },
                ],
            };
            let s = sql_of(&n);
            assert!(s.contains(" OR "));
        }

        #[test]
        fn not_wraps_child() {
            let n = RuleNode::Not {
                child: Box::new(RuleNode::Leaf {
                    predicate: Predicate::Liked,
                }),
            };
            let s = sql_of(&n);
            assert!(s.starts_with("NOT ("));
            assert!(s.ends_with(')'));
        }

        /// A relative window is turned into an absolute cut-off at
        /// build time. This pins the arithmetic — the unit is
        /// milliseconds, and getting it wrong by a factor of 1 000
        /// would silently widen "the last 30 days" to eighty years.
        #[test]
        fn a_window_binds_a_cut_off_in_milliseconds() {
            let binds = binds_of(&leaf(Predicate::PlayedInLastDays { value: 30 }));
            assert_eq!(binds.len(), 1);
            let BindValue::Int(cut_off) = binds[0] else {
                panic!("expected an integer bind");
            };
            assert_eq!(cut_off, NOW - 30 * 86_400_000);
        }

        /// An absurd window must clamp, not wrap. A negative cut-off
        /// still matches every play ever recorded; a wrapped one would
        /// land in the future and match none, which is the opposite of
        /// what "the last four billion days" asks for.
        #[test]
        fn an_absurd_window_clamps_instead_of_wrapping() {
            assert!(
                days_ago_ms(NOW, i64::MAX) < 0,
                "an absurd window must stay in the past"
            );
            assert_eq!(days_ago_ms(NOW, -5), NOW, "a negative window is no window");
        }

        /// Bounds on a nullable column carry their own NULL guard, so
        /// that a track missing the value falls out of the rule *and*
        /// into its negation rather than out of both.
        #[test]
        fn nullable_bounds_guard_their_column() {
            for p in [
                Predicate::SampleRateMin { value: 88_200 },
                Predicate::BitDepthMin { value: 24 },
                Predicate::DiscNumberIs { value: 2 },
                Predicate::HiRes,
            ] {
                let s = sql_of(&leaf(p.clone()));
                assert!(s.contains("IS NOT NULL"), "{p:?} must guard its NULLs: {s}");
            }
        }

        /// The tag name is a bind like any other value. It is the one
        /// place where user input names a *column's content* rather
        /// than being compared to one, so it is also the one place
        /// where an interpolated build would be tempting.
        #[test]
        fn a_tag_rule_binds_its_key() {
            let s = sql_of(&leaf(Predicate::TagContains {
                key: "COMPOSER".into(),
                value: "Glass".into(),
            }));
            assert!(
                !s.contains("COMPOSER"),
                "the key must not reach the SQL: {s}"
            );
            assert_eq!(
                s.matches('?').count(),
                2,
                "one bind for the key, one for the value"
            );
        }

        /// Both sides of a path rule are folded to `/`, so the same
        /// rule reads the same whichever separator is on either end.
        #[test]
        fn a_path_rule_folds_both_separators() {
            let binds = binds_of(&leaf(Predicate::PathContains {
                value: r"Live\Bootlegs".into(),
            }));
            let BindValue::Text(needle) = &binds[0] else {
                panic!("expected a text bind");
            };
            assert_eq!(needle, "%Live/Bootlegs%");
            assert!(
                sql_of(&leaf(Predicate::PathContains { value: "x".into() })).contains("REPLACE")
            );
        }

        /// Enough of the profile schema for the rule queries to run.
        ///
        /// Hand-rolled rather than the real migrations, which live in
        /// the app crate and are out of reach from `waveflow-core` —
        /// the same constraint the scanner and repository fixtures
        /// note. Only the columns the predicates read are here; the
        /// collation on `track_tag.key` is NOT optional decoration, it
        /// is what a tag rule depends on to match a key the scanner
        /// stored upper-cased.
        async fn fixture_pool(now_ms: i64) -> sqlx::SqlitePool {
            let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
            sqlx::raw_sql(
                "CREATE TABLE track (
                     id           INTEGER PRIMARY KEY,
                     title        TEXT NOT NULL,
                     file_path    TEXT NOT NULL,
                     duration_ms  INTEGER NOT NULL,
                     added_at     INTEGER NOT NULL,
                     is_available INTEGER NOT NULL DEFAULT 1,
                     sample_rate  INTEGER,
                     bit_depth    INTEGER,
                     disc_number  INTEGER,
                     year         INTEGER,
                     rating       INTEGER,
                     codec        TEXT,
                     album_id     INTEGER,
                     primary_artist INTEGER
                 );
                 CREATE TABLE play_event (
                     id        INTEGER PRIMARY KEY,
                     track_id  INTEGER,
                     played_at INTEGER NOT NULL
                 );
                 CREATE TABLE track_tag (
                     track_id INTEGER NOT NULL,
                     key      TEXT NOT NULL COLLATE NOCASE,
                     value    TEXT NOT NULL,
                     PRIMARY KEY (track_id, key)
                 );",
            )
            .execute(&pool)
            .await
            .unwrap();

            let day = 86_400_000_i64;
            // 1 — a Windows path, hi-res, disc 2, played five days ago,
            //     and carrying a composer tag written in upper case.
            // 2 — a POSIX path, lossy, no bit depth, last played more
            //     than a year ago.
            // 3 — matches nearly everything, but the file is gone.
            sqlx::query(
                "INSERT INTO track
                   (id, title, file_path, duration_ms, added_at, is_available,
                    sample_rate, bit_depth, disc_number)
                 VALUES
                   (1, 'Alpha', ?, 200000, ?, 1, 96000, 24, 2),
                   (2, 'Beta', '/home/u/Music/Studio/b.mp3', 200000, ?, 1,
                    44100, NULL, NULL),
                   (3, 'Gamma', '/home/u/Music/Live/Bootlegs/c.flac', 200000,
                    ?, 0, 96000, 24, 2)",
            )
            .bind(r"E:\Music\Live\Bootlegs\a.flac")
            .bind(now_ms - 10 * day)
            .bind(now_ms - 100 * day)
            .bind(now_ms - day)
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "INSERT INTO play_event (track_id, played_at) VALUES
                   (1, ?), (1, ?), (2, ?)",
            )
            .bind(now_ms - 5 * day)
            .bind(now_ms - 400 * day)
            .bind(now_ms - 400 * day)
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query("INSERT INTO track_tag (track_id, key, value) VALUES (1, 'COMPOSER', 'Philip Glass')")
                .execute(&pool)
                .await
                .unwrap();

            pool
        }

        fn rules(tree: RuleNode) -> CustomRules {
            CustomRules {
                tree,
                sort: None,
                limit: None,
            }
        }

        async fn matched(pool: &sqlx::SqlitePool, tree: RuleNode) -> Vec<i64> {
            run_query_at(pool, &rules(tree), NOW).await.unwrap()
        }

        /// The new predicates, against a real SQLite.
        ///
        /// The shape tests above assert on strings, which cannot tell a
        /// valid fragment from one SQLite refuses — and `REPLACE(…,
        /// '\\', '/')` is exactly the kind of fragment that either
        /// parses or does not. This one executes them.
        #[tokio::test]
        async fn the_new_predicates_run_and_select_what_they_claim() {
            let pool = fixture_pool(NOW).await;

            // A rule typed with forward slashes finds the file whose
            // path was written by Windows, and vice versa.
            assert_eq!(
                matched(
                    &pool,
                    leaf(Predicate::PathContains {
                        value: "Live/Bootlegs".into()
                    })
                )
                .await,
                vec![1],
                "the separator fold must work in both directions, and the \
                 unavailable track must stay out"
            );

            // Case-insensitivity comes from `LIKE` itself, not from a
            // collation: SQLite's `LIKE` ignores collating sequences.
            // This is the assertion that says so out loud, now that no
            // `COLLATE` sits in the fragment to suggest otherwise.
            assert_eq!(
                matched(
                    &pool,
                    leaf(Predicate::TitleContains {
                        value: "alpha".into()
                    })
                )
                .await,
                vec![1],
                "a lower-case needle must find an upper-case title"
            );

            assert_eq!(
                matched(&pool, leaf(Predicate::PlayCountMin { value: 2 })).await,
                vec![1]
            );
            assert_eq!(
                matched(&pool, leaf(Predicate::PlayCountMax { value: 1 })).await,
                vec![2]
            );
            assert_eq!(
                matched(&pool, leaf(Predicate::PlayedInLastDays { value: 30 })).await,
                vec![1]
            );
            assert_eq!(
                matched(&pool, leaf(Predicate::AddedInLastDays { value: 30 })).await,
                vec![1]
            );
            assert_eq!(
                matched(&pool, leaf(Predicate::SampleRateMin { value: 88_200 })).await,
                vec![1]
            );
            assert_eq!(
                matched(&pool, leaf(Predicate::DiscNumberIs { value: 2 })).await,
                vec![1]
            );

            // The NULL guard, end to end: track 2 has no bit depth, so
            // it falls out of the bound — and INTO its negation. Left
            // to SQLite's three-valued logic it would fall out of both.
            assert_eq!(
                matched(&pool, leaf(Predicate::BitDepthMin { value: 24 })).await,
                vec![1]
            );
            assert_eq!(
                matched(
                    &pool,
                    RuleNode::Not {
                        child: Box::new(leaf(Predicate::BitDepthMin { value: 24 }))
                    }
                )
                .await,
                vec![2],
                "a track with no bit depth belongs to the negation"
            );
            // The same question asked of `hi_res`, which is two bounds
            // at once: track 2 has a sample rate and no bit depth, and
            // is hi-res by neither.
            assert_eq!(
                matched(
                    &pool,
                    RuleNode::Not {
                        child: Box::new(leaf(Predicate::HiRes))
                    }
                )
                .await,
                vec![2],
                "a track that is hi-res by neither figure belongs to the negation"
            );

            // The key is stored upper-cased and asked for in lower —
            // `COLLATE NOCASE` on the column is what makes that work.
            assert_eq!(
                matched(
                    &pool,
                    leaf(Predicate::TagPresent {
                        key: "composer".into()
                    })
                )
                .await,
                vec![1]
            );
            assert_eq!(
                matched(
                    &pool,
                    leaf(Predicate::TagContains {
                        key: "COMPOSER".into(),
                        value: "glass".into(),
                    })
                )
                .await,
                vec![1]
            );
            assert!(matched(
                &pool,
                leaf(Predicate::TagContains {
                    key: "COMPOSER".into(),
                    value: "Reich".into(),
                })
            )
            .await
            .is_empty());

            // An underscore is a character in a path, not a wildcard —
            // and this is the assertion that proves the `ESCAPE` clause
            // reached the SQL, since a pattern escaped without one
            // matches the escape character literally.
            assert!(
                matched(
                    &pool,
                    leaf(Predicate::PathContains {
                        value: "M_sic".into()
                    })
                )
                .await
                .is_empty(),
                "an underscore must not match an arbitrary character"
            );
            assert_eq!(
                matched(
                    &pool,
                    leaf(Predicate::PathContains {
                        value: "Music".into()
                    })
                )
                .await,
                vec![1, 2],
                "and the same rule without it still matches"
            );
        }

        /// "Forgotten" — played once, but not lately. The template of
        /// the same name is this tree, and it is the one shape where a
        /// count and a window have to agree: without the count it would
        /// also sweep in everything never played at all.
        #[tokio::test]
        async fn forgotten_is_played_once_but_not_lately() {
            let pool = fixture_pool(NOW).await;
            let tree = RuleNode::All {
                children: vec![
                    leaf(Predicate::PlayCountMin { value: 1 }),
                    RuleNode::Not {
                        child: Box::new(leaf(Predicate::PlayedInLastDays { value: 180 })),
                    },
                ],
            };
            assert_eq!(matched(&pool, tree).await, vec![2]);
        }

        /// The counter reports the matches *above* the limit, which is
        /// the number a truncated list cannot give and the reason the
        /// editor shows both.
        #[tokio::test]
        async fn the_count_separates_what_matches_from_what_is_kept() {
            let pool = fixture_pool(NOW).await;
            let mut r = rules(RuleNode::All { children: vec![] });
            let all = count_matches_at(&pool, &r, NOW).await.unwrap();
            assert_eq!(
                all,
                RulesCount { total: 2, kept: 2 },
                "the unavailable track counts for neither"
            );

            r.limit = Some(1);
            assert_eq!(
                count_matches_at(&pool, &r, NOW).await.unwrap(),
                RulesCount { total: 2, kept: 1 }
            );
            assert_eq!(
                run_query_at(&pool, &r, NOW).await.unwrap().len(),
                1,
                "`kept` must be what the query actually returns"
            );
        }

        #[test]
        fn nested_tree_renders_full_expression() {
            // (artist=X OR artist=Y) AND year >= 2000 AND NOT liked
            let n = RuleNode::All {
                children: vec![
                    RuleNode::Any {
                        children: vec![
                            RuleNode::Leaf {
                                predicate: Predicate::ArtistContains { value: "X".into() },
                            },
                            RuleNode::Leaf {
                                predicate: Predicate::ArtistContains { value: "Y".into() },
                            },
                        ],
                    },
                    RuleNode::Leaf {
                        predicate: Predicate::YearMin { value: 2000 },
                    },
                    RuleNode::Not {
                        child: Box::new(RuleNode::Leaf {
                            predicate: Predicate::Liked,
                        }),
                    },
                ],
            };
            let s = sql_of(&n);
            assert!(s.contains(" OR "));
            assert!(s.contains(" AND "));
            assert!(s.contains("NOT ("));
        }
    }

    #[test]
    fn migrate_v1_flat_rules_to_tree() {
        // Old-shape JSON — what's already sitting in user DBs.
        let v1 = r#"{
            "title_contains": "foo",
            "year_min": 2020,
            "genre_ids": [1, 2, 3],
            "liked_only": true,
            "sort": "title_asc",
            "limit": 100
        }"#;
        let rules: CustomRules = serde_json::from_str(v1).unwrap();
        let RuleNode::All { children } = &rules.tree else {
            panic!("expected All root, got {:?}", rules.tree);
        };
        // 4 children: title, year_min, genre Any-group, liked.
        assert_eq!(children.len(), 4);
        // genre_ids must be wrapped in `Any` of three leaves.
        let genre_group = children.iter().find_map(|c| match c {
            RuleNode::Any { children } if children.len() == 3 => Some(children),
            _ => None,
        });
        assert!(
            genre_group.is_some(),
            "genre_ids should migrate to Any group of 3 leaves"
        );
        assert!(matches!(rules.sort, Some(CustomSort::TitleAsc)));
        assert_eq!(rules.limit, Some(100));
    }

    #[test]
    fn v2_tree_round_trips() {
        let v2 = r#"{
            "tree": {
                "type": "all",
                "children": [
                    {"type": "leaf", "predicate": {"kind": "liked"}},
                    {"type": "not", "child": {"type": "leaf",
                        "predicate": {"kind": "hi_res"}}}
                ]
            },
            "sort": "random",
            "limit": 50
        }"#;
        let rules: CustomRules = serde_json::from_str(v2).unwrap();
        let RuleNode::All { children } = &rules.tree else {
            panic!("expected All root");
        };
        assert_eq!(children.len(), 2);
        assert!(matches!(rules.sort, Some(CustomSort::Random)));
    }
}

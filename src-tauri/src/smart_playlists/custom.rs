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
use sqlx::SqlitePool;

use crate::error::{AppError, AppResult};

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
// SQL builder
// =============================================================================

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
fn build_node_sql(node: &RuleNode, binds: &mut Vec<BindValue>) -> String {
    match node {
        RuleNode::All { children } => {
            if children.is_empty() {
                return "1=1".to_string();
            }
            let parts: Vec<String> = children.iter().map(|c| build_node_sql(c, binds)).collect();
            format!("({})", parts.join(" AND "))
        }
        RuleNode::Any { children } => {
            if children.is_empty() {
                return "0=1".to_string();
            }
            let parts: Vec<String> = children.iter().map(|c| build_node_sql(c, binds)).collect();
            format!("({})", parts.join(" OR "))
        }
        RuleNode::Not { child } => {
            let inner = build_node_sql(child, binds);
            format!("NOT ({inner})")
        }
        RuleNode::Leaf { predicate } => build_predicate_sql(predicate, binds),
    }
}

/// One SQL fragment per predicate. Every join-needing predicate uses
/// an `EXISTS` subquery so the tree can be nested arbitrarily without
/// row duplication at the top level.
fn build_predicate_sql(pred: &Predicate, binds: &mut Vec<BindValue>) -> String {
    match pred {
        Predicate::TitleContains { value } => {
            binds.push(BindValue::Text(format!("%{}%", value.trim())));
            "t.title LIKE ? COLLATE NOCASE".to_string()
        }
        Predicate::ArtistContains { value } => {
            binds.push(BindValue::Text(format!("%{}%", value.trim())));
            "EXISTS (SELECT 1 FROM track_artist ta JOIN artist ar ON ar.id = ta.artist_id \
             WHERE ta.track_id = t.id AND ar.name LIKE ? COLLATE NOCASE)"
                .to_string()
        }
        Predicate::AlbumContains { value } => {
            binds.push(BindValue::Text(format!("%{}%", value.trim())));
            "EXISTS (SELECT 1 FROM album WHERE album.id = t.album_id AND album.title LIKE ? COLLATE NOCASE)"
                .to_string()
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
        Predicate::HiRes => "(t.sample_rate >= 88200 OR t.bit_depth >= 24)".to_string(),
        Predicate::Liked => {
            "EXISTS (SELECT 1 FROM liked_track lt WHERE lt.track_id = t.id)".to_string()
        }
        Predicate::RatingMin { value } => {
            binds.push(BindValue::Int((*value).clamp(1, 255)));
            "(t.rating IS NOT NULL AND t.rating >= ?)".to_string()
        }
    }
}

// =============================================================================
// Public materialize / query
// =============================================================================

/// Re-materialize the playlist's tracks from its rule set. Wipes
/// `playlist_track` rows for the playlist, runs the rule query, then
/// re-inserts the results in the sorted order. The rule set is read
/// from `playlist.smart_rules` so this command is idempotent (calling
/// it twice yields the same membership unless the library changed).
pub async fn materialize(
    pool: &SqlitePool,
    playlist_id: i64,
    rules: &CustomRules,
) -> AppResult<i64> {
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

/// Resolve the rule set into a list of track ids in the canonical sort
/// order. Public for the dry-run "Preview" button in the rule editor.
pub async fn run_query(pool: &SqlitePool, rules: &CustomRules) -> AppResult<Vec<i64>> {
    let mut binds = Vec::<BindValue>::new();
    let tree_where = build_node_sql(&rules.tree, &mut binds);

    let mut sql = String::from("SELECT t.id FROM track t WHERE t.is_available = 1 AND ");
    sql.push_str(&tree_where);
    sql.push_str(" ORDER BY ");
    sql.push_str(order_by_sql(
        rules.sort.as_ref().unwrap_or(&CustomSort::AddedDesc),
    ));

    let limit = rules.limit.unwrap_or(HARD_LIMIT).clamp(1, HARD_LIMIT);
    sql.push_str(" LIMIT ?");
    binds.push(BindValue::Int(limit));

    let mut q = sqlx::query_scalar::<_, i64>(&sql);
    for b in binds {
        q = match b {
            BindValue::Int(v) => q.bind(v),
            BindValue::Real(v) => q.bind(v),
            BindValue::Text(v) => q.bind(v),
        };
    }
    let rows = q
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Other(format!("custom smart playlist query failed: {e}")))?;
    Ok(rows)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build SQL string from a tree (binds are discarded for
    /// readability — the tests only assert on the textual shape).
    fn sql_of(node: &RuleNode) -> String {
        let mut binds = Vec::new();
        build_node_sql(node, &mut binds)
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
        assert_eq!(sql_of(&n), "t.title LIKE ? COLLATE NOCASE");
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

//! Pure-string helpers shared between [`super::extract`] and
//! [`super::upserts`]. Lives in its own module so the postgres-only
//! build (which skips `upserts`) can still consume `canonical_name`
//! from `extract`.

/// Normalize a title/name for dedup purposes: lowercase, strip punctuation
/// and collapse whitespace. Good enough to match "The Beatles" / "THE  BEATLES"
/// or "the beatles!" onto a single canonical key without pulling in a proper
/// Unicode normalization library.
pub fn canonical_name(s: &str) -> String {
    s.trim()
        .chars()
        .flat_map(|c| c.to_lowercase())
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

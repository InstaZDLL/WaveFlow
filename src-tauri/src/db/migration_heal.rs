//! Self-healing for sqlx migration checksum mismatches caused by
//! line-ending drift on Windows.
//!
//! ## The problem
//!
//! `sqlx::migrate!` reads each `.sql` file at compile time, computes a
//! SHA-384, and embeds it in the binary. On first apply, sqlx writes
//! that hash into `_sqlx_migrations.checksum`. On every subsequent boot
//! it re-reads the embedded hash and panics if the stored row differs:
//!
//! > `migration <id> was previously applied but has been modified`
//!
//! The intent — protect users from a maintainer silently editing a
//! merged migration — is correct. The trouble is the hash is computed
//! over **raw bytes**, so a file that round-trips between LF and CRLF
//! changes its checksum even though the SQL is byte-for-byte identical
//! after newline normalization.
//!
//! On Windows with `core.autocrlf=true` (Git for Windows installer
//! default), a fresh checkout of a new `.sql` file can land as CRLF for
//! a brief window — long enough to be applied to the user's DB — before
//! `.gitattributes` (which forces `*.sql text eol=lf` for us) or
//! `git add --renormalize` restores LF. The stored checksum now points
//! at the CRLF variant; the next boot reads the LF file and panics.
//!
//! ## What this module does
//!
//! Before each `sqlx::migrate!().run()`, for every row in
//! `_sqlx_migrations` whose stored checksum doesn't match the
//! compiled-in migration's checksum:
//!
//! 1. Recompute SHA-384 of the migration SQL after normalizing line
//!    endings to LF, and again after normalizing to CRLF.
//! 2. If either matches the stored row, the divergence is
//!    line-ending-only. Overwrite the stored row with the canonical
//!    (compiled-in) hash and emit a `tracing::warn!`.
//! 3. Otherwise, leave the row alone — sqlx will panic, as it should,
//!    because that's a real SQL change.
//!
//! This is safe: it never accepts a SQL mutation, only confirms the
//! same statements would still parse if the developer happened to be
//! on the other newline platform. A maliciously edited migration whose
//! LF-or-CRLF hash happens to collide with the previous checksum is a
//! SHA-384 second-preimage attack — not a realistic risk.

use sqlx::{migrate::Migrator, SqlitePool};

use crate::error::AppResult;

/// Reconcile `_sqlx_migrations.checksum` rows against the compiled-in
/// migrator. Returns the number of rows healed (0 on a fresh DB or when
/// nothing needed fixing). Always safe to call before
/// [`Migrator::run`] — it short-circuits when `_sqlx_migrations`
/// doesn't exist yet.
pub async fn heal_line_ending_drift(
    pool: &SqlitePool,
    migrator: &Migrator,
) -> AppResult<usize> {
    // Fresh database: `_sqlx_migrations` is created by `Migrator::run`
    // itself, so on first boot there's nothing to reconcile.
    let table_present: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?;
    if table_present.is_none() {
        return Ok(0);
    }

    let stored: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations")
            .fetch_all(pool)
            .await?;

    let mut healed = 0usize;
    for (version, stored_ck) in stored {
        let Some(migration) = migrator.iter().find(|m| m.version == version) else {
            // Stored row with no matching compiled-in migration. Don't
            // touch it — sqlx will surface it through its own error
            // path if it matters.
            continue;
        };

        let canonical = migration.checksum.as_ref();
        if stored_ck.as_slice() == canonical {
            continue;
        }

        let sql_bytes = migration.sql.as_bytes();
        let lf = normalize_to_lf(sql_bytes);
        let crlf = lf_to_crlf(&lf);

        let lf_hash = sha384(&lf);
        let crlf_hash = sha384(&crlf);

        if stored_ck.as_slice() == lf_hash.as_slice()
            || stored_ck.as_slice() == crlf_hash.as_slice()
        {
            sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
                .bind(canonical)
                .bind(version)
                .execute(pool)
                .await?;
            healed += 1;
            tracing::warn!(
                version,
                "self-healed migration checksum drift (line-ending mismatch — stored hash matched LF or CRLF variant of the same SQL)"
            );
        }
        // else: real divergence. Falls through to sqlx::migrate! which
        // will panic with the usual "previously applied but modified"
        // message, which is the correct behavior — a merged migration
        // was edited.
    }

    if healed > 0 {
        tracing::info!(healed, "reconciled migration checksums after line-ending drift");
    }
    Ok(healed)
}

/// Collapse any CRLF in `input` to LF, leaving lone CR and lone LF
/// alone. Mirrors what `.gitattributes`' `text eol=lf` does on
/// checkout.
fn normalize_to_lf(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'\r' && i + 1 < input.len() && input[i + 1] == b'\n' {
            // skip the \r, the upcoming \n will land on its own
            i += 1;
            continue;
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

/// Expand every LF in `lf` into CRLF, leaving anything else (notably
/// pre-existing CR) alone. Inverse partner of [`normalize_to_lf`] for
/// the second hash we compare against.
fn lf_to_crlf(lf: &[u8]) -> Vec<u8> {
    // Pre-count newlines so the destination Vec lands at exact capacity.
    // Typical SQL is 5–10 % `\n`; the previous +1/32 margin tripped
    // reallocations on every realistic migration, while a blanket ×2
    // would waste roughly half the allocation. A linear pre-pass over
    // a few KB of bytes is free.
    let lf_count = lf.iter().filter(|&&b| b == b'\n').count();
    let mut out = Vec::with_capacity(lf.len() + lf_count);
    for &b in lf {
        if b == b'\n' {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

fn sha384(data: &[u8]) -> [u8; 48] {
    use sha2::{Digest, Sha384};
    let mut hasher = Sha384::new();
    hasher.update(data);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lf_to_crlf_round_trips_through_normalize() {
        let lf = b"a\nb\nc\n";
        let crlf = lf_to_crlf(lf);
        assert_eq!(crlf, b"a\r\nb\r\nc\r\n");
        assert_eq!(normalize_to_lf(&crlf), lf);
    }

    #[test]
    fn normalize_preserves_lone_cr() {
        // An old Mac classic line ending or a literal embedded \r in a
        // string should NOT be collapsed — only the CRLF pair.
        let mixed = b"a\rb\r\nc";
        assert_eq!(normalize_to_lf(mixed), b"a\rb\nc");
    }

    #[test]
    fn normalize_is_idempotent_on_lf() {
        let lf = b"line1\nline2\n";
        assert_eq!(normalize_to_lf(lf), lf);
    }

    #[test]
    fn lf_and_crlf_variants_hash_differently() {
        // Sanity: if these collided the whole heuristic would be a
        // no-op. SHA-384 over distinct inputs must give distinct
        // outputs (modulo cosmic-ray collisions).
        let lf = b"create table foo (id int);\n";
        let crlf = lf_to_crlf(lf);
        assert_ne!(sha384(lf), sha384(&crlf));
    }
}

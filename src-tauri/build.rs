use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    tauri_build::build();
    generate_changelog();
}

/// Build-time changelog generator.
///
/// Runs `git log` once at compile time, parses the conventional-commit
/// header (`type(scope): subject`) and writes a JSON array next to the
/// build artefacts. The Tauri backend embeds the result via
/// `include_str!` so the shipped binary carries its own changelog —
/// users without git installed still see it.
///
/// Failure is non-fatal: if git is missing or the working dir isn't a
/// repository, an empty JSON array is written and the About view shows
/// nothing under "Changelog".
fn generate_changelog() {
    let out_dir: PathBuf = env::var_os("OUT_DIR")
        .expect("OUT_DIR not set")
        .into();
    let dest = out_dir.join("changelog.json");

    // Re-run when HEAD moves (new commit, branch switch). We don't
    // watch every ref because in dev that would mean re-emitting the
    // file on every fetch.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads");

    let entries = read_git_log().unwrap_or_default();
    let json = serde_json::to_string(&entries).unwrap_or_else(|_| "[]".into());
    if let Err(err) = fs::write(&dest, json) {
        println!("cargo:warning=changelog: failed to write {}: {err}", dest.display());
        let _ = fs::write(&dest, "[]");
    }
}

fn read_git_log() -> Option<Vec<ChangelogEntry>> {
    // %h hash, %s subject, %cI committer date ISO-8601 strict.
    // Fields separated by U+001F (unit), records by U+001E (record).
    let output = Command::new("git")
        .args([
            "log",
            "--max-count=300",
            "--no-merges",
            "--pretty=format:%h\x1f%s\x1f%cI\x1e",
        ])
        .current_dir("..")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let entries = raw
        .split('\x1e')
        .filter_map(|record| {
            let trimmed = record.trim();
            if trimmed.is_empty() {
                return None;
            }
            let mut parts = trimmed.splitn(3, '\x1f');
            let hash = parts.next()?.trim().to_string();
            let subject = parts.next()?.trim().to_string();
            let date = parts.next()?.trim().to_string();
            parse_entry(hash, subject, date)
        })
        .collect();
    Some(entries)
}

#[derive(serde::Serialize)]
struct ChangelogEntry {
    hash: String,
    /// Conventional-commit type (`feat`, `fix`, `chore`, …).
    #[serde(rename = "type")]
    kind: String,
    scope: Option<String>,
    subject: String,
    breaking: bool,
    /// ISO-8601 committer date.
    date: String,
}

/// Parse a conventional-commit subject line. Returns `None` for commits
/// that don't match the format so they're omitted from the rendered
/// changelog (keeps marketing-style noise out).
fn parse_entry(hash: String, subject: String, date: String) -> Option<ChangelogEntry> {
    let (header, rest) = subject.split_once(": ")?;
    if rest.is_empty() {
        return None;
    }
    let (kind, scope, breaking) = parse_header(header)?;
    Some(ChangelogEntry {
        hash,
        kind: kind.to_string(),
        scope: scope.map(str::to_string),
        subject: rest.to_string(),
        breaking,
        date,
    })
}

/// Header is `type` or `type(scope)`, optionally followed by `!` to
/// flag a breaking change. Returns the type, the optional scope and
/// the breaking flag.
fn parse_header(header: &str) -> Option<(&str, Option<&str>, bool)> {
    let (head, breaking) = match header.strip_suffix('!') {
        Some(s) => (s, true),
        None => (header, false),
    };
    if let Some((kind, rest)) = head.split_once('(') {
        let scope = rest.strip_suffix(')')?;
        Some((kind, Some(scope), breaking))
    } else {
        Some((head, None, breaking))
    }
}

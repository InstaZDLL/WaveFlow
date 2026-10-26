//! Playlists brought in from elsewhere: an M3U, a CSV or JSON export, a
//! public Deezer playlist.
//!
//! Every source is read into the same list of [`ImportEntry`], in the
//! playlist's own order, and every entry goes through the same
//! [`matching::LibraryIndex`]. An entry the library holds becomes a
//! `playlist_track` row; one it does not becomes a **placeholder**
//! (`playlist_placeholder`), which keeps its place in the order and its
//! metadata, and which a later scan turns into the real track when that
//! track shows up. A placeholder is never presented as something the user
//! owns: it is not counted, not timed, not queued.
//!
//! Parsing is here, without a network or a filesystem, so the rules can be
//! tested on their own; reading the file, fetching Deezer and writing the
//! rows belong to the caller.

pub mod csv;
pub mod deezer_link;
pub mod json;
pub mod m3u;
pub mod matching;

/// Most entries one import keeps. A real playlist is far below this; the
/// cap is here so a file that is not one cannot fill the database.
pub const MAX_ENTRIES: usize = 10_000;

/// Where a placeholder came from — the `playlist_placeholder.source`
/// column, whose `CHECK` lists the same four strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportSource {
    M3u,
    Csv,
    Json,
    Deezer,
}

impl ImportSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::M3u => "m3u",
            Self::Csv => "csv",
            Self::Json => "json",
            Self::Deezer => "deezer",
        }
    }
}

/// One entry of an imported playlist, before it is matched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportEntry {
    pub title: String,
    /// As the source wrote it — possibly several names in one string
    /// ("A, B", "A; B", "A feat. B"). Kept whole for display; the matcher
    /// splits it.
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<i64>,
    /// Already through [`normalize_isrc`].
    pub isrc: Option<String>,
    /// The file an M3U entry names, resolved against the playlist's own
    /// folder. `None` for every other source.
    pub locator: Option<String>,
}

/// A source read into entries, with the name it gives itself, if any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedPlaylist {
    pub name: Option<String>,
    pub entries: Vec<ImportEntry>,
}

/// Why a file gave no playlist.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// A CSV whose header names no title column — there would be nothing
    /// to show, let alone to match.
    #[error("no title column in the header")]
    NoTitleColumn,
    #[error("not a playlist JSON: {0}")]
    Json(String),
    #[error("the playlist has no entry")]
    Empty,
}

/// An ISRC in the form the matcher compares: twelve characters, upper
/// case, the hyphens and spaces some exports insert removed. Anything that
/// is not twelve letters and digits afterwards is not an ISRC and is
/// dropped rather than compared.
pub fn normalize_isrc(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !matches!(c, '-' | ' ' | '\u{a0}'))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (cleaned.len() == 12 && cleaned.chars().all(|c| c.is_ascii_alphanumeric())).then_some(cleaned)
}

/// A duration as exports spell it: `m:ss` / `h:mm:ss`, or a plain number.
///
/// A plain number is seconds unless `as_ms` says the column is in
/// milliseconds — or unless it is above 20 000: no track runs five and a
/// half hours, and an export that writes milliseconds in a column called
/// "duration" is common enough to be worth reading right.
pub fn parse_duration(raw: &str, as_ms: bool) -> Option<i64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.contains(':') {
        let mut total: i64 = 0;
        for part in raw.split(':') {
            let value: f64 = part.trim().parse().ok()?;
            if !(0.0..60.0).contains(&value) && total > 0 {
                return None;
            }
            total = total.checked_mul(60)?;
            total = total.checked_add(value.floor() as i64)?;
        }
        return Some(total * 1000).filter(|ms| *ms > 0);
    }
    let value: f64 = raw.parse().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let ms = if as_ms || value > 20_000.0 {
        value
    } else {
        value * 1000.0
    };
    Some(ms.round() as i64)
}

/// Build a comparable key from a filesystem path. We canonicalize
/// when possible (resolves symlinks, fixes case, tightens drive
/// letters) then strip the `\\?\` and `\\?\UNC\` extended-length
/// prefixes Windows' `canonicalize` adds. Falls back to the input
/// path when canonicalize fails so library-relative .m3u entries can
/// still match scanned tracks even if the file isn't currently
/// mounted.
///
/// **Platform-aware case folding**: filesystems are case-insensitive on
/// Windows + macOS (HFS+/APFS default) and case-sensitive on Linux. We
/// lowercase only on Windows so a Linux library where `Song.flac` and
/// `song.flac` are two distinct files (rare but legal) doesn't collide
/// during the M3U → DB match. macOS is treated as case-sensitive too —
/// the rare case-sensitive HFS+/APFS volume gets a correct match, and
/// the common case-insensitive one only suffers when the M3U casing
/// disagrees with the on-disk casing (which `canonicalize` usually
/// fixes anyway).
pub fn path_key(p: &std::path::Path) -> String {
    let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = canon.to_string_lossy().to_string();
    #[cfg(windows)]
    {
        // Windows: strip `\\?\` / `\\?\UNC\` prefixes that canonicalize
        // adds (extended-length paths), then lowercase since NTFS is
        // case-insensitive by default. Byte-level prefix match used so
        // some shells that mangle raw strings can't drop a backslash.
        let bytes = s.as_bytes();
        let has_verbatim = bytes.len() >= 4
            && bytes[0] == b'\\'
            && bytes[1] == b'\\'
            && bytes[2] == b'?'
            && bytes[3] == b'\\';
        let has_unc = has_verbatim
            && bytes.len() >= 8
            && (bytes[4] == b'U' || bytes[4] == b'u')
            && (bytes[5] == b'N' || bytes[5] == b'n')
            && (bytes[6] == b'C' || bytes[6] == b'c')
            && bytes[7] == b'\\';
        if has_unc {
            // \\?\UNC\server\share\... → \\server\share\...
            return format!("\\\\{}", &s[8..]).to_lowercase();
        }
        if has_verbatim {
            // \\?\C:\... → C:\...
            return s[4..].to_lowercase();
        }
        s.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        // Linux / macOS: case-sensitive matching. No extended-length
        // prefix to strip — `canonicalize` returns plain absolute paths.
        s
    }
}

/// Trim a field and turn an empty one into `None`.
pub(crate) fn non_empty(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_isrc_is_compared_without_its_hyphens_and_case() {
        assert_eq!(
            normalize_isrc("gb-n9y-11-00088").as_deref(),
            Some("GBN9Y1100088")
        );
        assert_eq!(
            normalize_isrc(" USRC17607839 ").as_deref(),
            Some("USRC17607839")
        );
        assert_eq!(normalize_isrc("not an isrc"), None);
        assert_eq!(normalize_isrc("USRC1760783"), None);
        assert_eq!(normalize_isrc(""), None);
    }

    #[test]
    fn durations_are_read_the_way_exports_write_them() {
        assert_eq!(parse_duration("3:45", false), Some(225_000));
        assert_eq!(parse_duration("1:02:03", false), Some(3_723_000));
        assert_eq!(parse_duration("225", false), Some(225_000));
        assert_eq!(parse_duration("225.4", false), Some(225_400));
        assert_eq!(parse_duration("225000", true), Some(225_000));
        // Milliseconds in a column that does not say so.
        assert_eq!(parse_duration("225000", false), Some(225_000));
        assert_eq!(parse_duration("", false), None);
        assert_eq!(parse_duration("-1", false), None);
        assert_eq!(parse_duration("abc", false), None);
        assert_eq!(parse_duration("3:75", false), None);
    }
}

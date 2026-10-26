//! A playlist as an `.m3u` / `.m3u8` file.
//!
//! The paths are what an M3U is for, and the importer matches them first.
//! The `#EXTINF:<seconds>,<artist> - <title>` line in front of a path is
//! what lets an entry the library does not hold stay in the playlist as a
//! placeholder with a name, rather than as a bare path.

use super::non_empty;

/// One entry, in the file's order. `path` is as written: absolute, or
/// relative to the playlist's folder, which the caller resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct M3uEntry {
    pub path: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration_ms: Option<i64>,
}

/// The `#PLAYLIST:` name, when the file gives one, and its entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct M3u {
    pub name: Option<String>,
    pub entries: Vec<M3uEntry>,
}

/// `#EXTINF:<seconds>[ key="value"…],<display>` → (duration, display).
/// A duration of `-1` (unknown, the convention for streams) or anything
/// unparsable is no duration.
fn parse_extinf(rest: &str) -> (Option<i64>, Option<String>) {
    let (head, display) = rest.split_once(',').unwrap_or((rest, ""));
    let seconds = head.split_whitespace().next().unwrap_or("");
    let duration_ms = seconds
        .parse::<f64>()
        .ok()
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| (s * 1000.0).round() as i64);
    (duration_ms, non_empty(display))
}

/// Split `Artist - Title`, the display convention every writer follows.
/// Without the separator the whole string is the title.
fn split_display(display: &str) -> (Option<String>, String) {
    match display.split_once(" - ") {
        Some((artist, title)) if !artist.trim().is_empty() && !title.trim().is_empty() => {
            (non_empty(artist), title.trim().to_string())
        }
        _ => (None, display.trim().to_string()),
    }
}

pub fn parse(text: &str) -> M3u {
    let mut out = M3u::default();
    let mut pending: Option<(Option<i64>, Option<String>)> = None;
    for raw_line in text.lines() {
        // BOMs sneak in on Windows-edited m3u8 files.
        let line = raw_line.trim_start_matches('\u{feff}').trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            pending = Some(parse_extinf(rest));
            continue;
        }
        if let Some(name) = line.strip_prefix("#PLAYLIST:") {
            out.name = non_empty(name);
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let (duration_ms, display) = pending.take().unwrap_or((None, None));
        let (artist, title) = match display.as_deref().map(split_display) {
            Some((artist, title)) => (artist, Some(title)),
            None => (None, None),
        };
        out.entries.push(M3uEntry {
            path: line.to_string(),
            title,
            artist,
            duration_ms,
        });
        if out.entries.len() >= super::MAX_ENTRIES {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extinf_lines_name_the_entry_that_follows() {
        let text = "\u{feff}#EXTM3U\n#PLAYLIST:Evening\n#EXTINF:225,Daft Punk - One More Time\n\
                    C:\\Music\\one.flac\nrelative/two.mp3\n#EXTINF:-1,Just a title\nthree.ogg\n";
        let m3u = parse(text);
        assert_eq!(m3u.name.as_deref(), Some("Evening"));
        assert_eq!(m3u.entries.len(), 3);
        assert_eq!(
            m3u.entries[0],
            M3uEntry {
                path: "C:\\Music\\one.flac".into(),
                title: Some("One More Time".into()),
                artist: Some("Daft Punk".into()),
                duration_ms: Some(225_000),
            }
        );
        // No #EXTINF: the line before it belonged to the previous entry.
        assert_eq!(m3u.entries[1].title, None);
        assert_eq!(m3u.entries[2].title.as_deref(), Some("Just a title"));
        assert_eq!(m3u.entries[2].artist, None);
        assert_eq!(m3u.entries[2].duration_ms, None);
    }

    #[test]
    fn extinf_attributes_are_skipped() {
        let (duration, display) = parse_extinf("123 tvg-id=\"x\",A - B");
        assert_eq!(duration, Some(123_000));
        assert_eq!(display.as_deref(), Some("A - B"));
    }
}

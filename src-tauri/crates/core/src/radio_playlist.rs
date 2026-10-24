//! Station playlists: the `.m3u` / `.m3u8` and `.pls` files a radio
//! hands out instead of its stream.
//!
//! Adding a station by hand means accepting what its website links to,
//! and that is as often a small playlist naming the stream as the stream
//! itself — `listen.pls`, `live.m3u`. Both formats are a few lines of
//! text, parsed here without a network or a filesystem so the rules can
//! be tested on their own; fetching and resolving belong to the caller.
//!
//! **HLS is refused, not parsed.** An `.m3u8` can also be an HTTP Live
//! Streaming playlist — a list of short media segments, not a list of
//! stations — and the engine plays a continuous stream, not segments.
//! It is recognised by its `#EXT-X-` tags and reported as such, so the
//! user is told why rather than handed a station that plays two seconds.

/// One stream a playlist names, in the order it names them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PlaylistEntry {
    /// As written: absolute, or relative to the playlist's own URL. The
    /// caller resolves and keeps only `http` / `https`.
    pub url: String,
    /// `#EXTINF:-1,Title` / `TitleN=` — the station's name, when the
    /// playlist gives one.
    pub title: Option<String>,
}

/// Why a playlist gave no station.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistError {
    /// An HTTP Live Streaming playlist: segments, not a station.
    Hls,
    /// Nothing in it names a stream.
    Empty,
}

impl std::fmt::Display for PlaylistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Hls => "an HLS playlist, not a station",
            Self::Empty => "the playlist names no stream",
        })
    }
}

impl std::error::Error for PlaylistError {}

/// Whether `path` (a URL path or a file name) is spelled like a playlist.
pub fn has_playlist_extension(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".m3u", ".m3u8", ".pls"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Whether `url` is an absolute `http` / `https` URL — the only kind a
/// station can be. Case-insensitive on the scheme, as URLs are.
pub fn is_http_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Parse a playlist, whichever of the two it is.
///
/// `.pls` announces itself with a `[playlist]` header; everything else is
/// read as M3U, which is also what a bare list of URLs is.
pub fn parse_playlist(text: &str) -> Result<Vec<PlaylistEntry>, PlaylistError> {
    let text = text.trim_start_matches('\u{feff}');
    let first = text.lines().map(str::trim).find(|l| !l.is_empty());
    let entries = if first.is_some_and(|l| l.eq_ignore_ascii_case("[playlist]")) {
        parse_pls(text)
    } else {
        parse_m3u(text)?
    };
    if entries.is_empty() {
        Err(PlaylistError::Empty)
    } else {
        Ok(entries)
    }
}

fn parse_m3u(text: &str) -> Result<Vec<PlaylistEntry>, PlaylistError> {
    let mut out = Vec::new();
    let mut title: Option<String> = None;
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        if strip_prefix_ignore_case(line, "#EXT-X-").is_some() {
            return Err(PlaylistError::Hls);
        }
        if let Some(info) = strip_prefix_ignore_case(line, "#EXTINF:") {
            // `#EXTINF:<duration>,<title>` — the title is everything after
            // the first comma, commas included.
            title = info
                .split_once(',')
                .map(|(_, t)| t.trim())
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        out.push(PlaylistEntry {
            url: line.to_string(),
            title: title.take(),
        });
    }
    Ok(out)
}

fn parse_pls(text: &str) -> Vec<PlaylistEntry> {
    use std::collections::BTreeMap;
    // `FileN=` and `TitleN=`, numbered from 1 and in any order; the
    // number is what pairs a title with its file.
    let mut files: BTreeMap<u32, String> = BTreeMap::new();
    let mut titles: BTreeMap<u32, String> = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if let Some(n) = numbered(key, "file") {
            files.insert(n, value.to_string());
        } else if let Some(n) = numbered(key, "title") {
            titles.insert(n, value.to_string());
        }
    }
    files
        .into_iter()
        .map(|(n, url)| PlaylistEntry {
            url,
            title: titles.remove(&n),
        })
        .collect()
}

/// `File3` → `Some(3)` for `name = "file"`, ignoring case.
fn numbered(key: &str, name: &str) -> Option<u32> {
    strip_prefix_ignore_case(key, name)?.parse().ok()
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len()
        && s.is_char_boundary(prefix.len())
        && s[..prefix.len()].eq_ignore_ascii_case(prefix)
    {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(entries: &[PlaylistEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.url.as_str()).collect()
    }

    #[test]
    fn an_extended_m3u_gives_its_streams_and_their_titles() {
        let text = "#EXTM3U\r\n#EXTINF:-1,Radio One, the first\r\nhttp://a.example/live\r\n\r\n# a comment\r\nhttps://b.example/mp3\r\n";
        let entries = parse_playlist(text).unwrap();
        assert_eq!(
            urls(&entries),
            ["http://a.example/live", "https://b.example/mp3"]
        );
        assert_eq!(entries[0].title.as_deref(), Some("Radio One, the first"));
        assert_eq!(
            entries[1].title, None,
            "a title belongs to the next line only"
        );
    }

    /// A comment in another script: the prefix test must not cut a
    /// character in half.
    #[test]
    fn a_non_ascii_line_is_read_not_split() {
        let text = "#EXTM3U\n# Радио Рекорд\n# 日本のラジオ\nhttp://a.example/live\n";
        let entries = parse_playlist(text).unwrap();
        assert_eq!(urls(&entries), ["http://a.example/live"]);
    }

    #[test]
    fn a_bare_list_of_urls_is_an_m3u() {
        let entries = parse_playlist("\u{feff}http://a.example/stream\n").unwrap();
        assert_eq!(urls(&entries), ["http://a.example/stream"]);
    }

    #[test]
    fn a_pls_pairs_titles_with_files_by_number() {
        let text = "[Playlist]\nNumberOfEntries=2\nFile2=http://mirror.example/live\nTitle1=Jazz FM\nfile1=http://main.example/live\nLength1=-1\nVersion=2\n";
        let entries = parse_playlist(text).unwrap();
        assert_eq!(
            urls(&entries),
            ["http://main.example/live", "http://mirror.example/live"]
        );
        assert_eq!(entries[0].title.as_deref(), Some("Jazz FM"));
        assert_eq!(entries[1].title, None);
    }

    #[test]
    fn hls_is_recognised_and_refused() {
        let text = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXTINF:6.0,\nseg1.ts\n";
        assert_eq!(parse_playlist(text), Err(PlaylistError::Hls));
    }

    #[test]
    fn a_playlist_naming_nothing_is_empty() {
        assert_eq!(
            parse_playlist("#EXTM3U\n# nothing\n"),
            Err(PlaylistError::Empty)
        );
        assert_eq!(
            parse_playlist("[playlist]\nNumberOfEntries=0\n"),
            Err(PlaylistError::Empty)
        );
        assert_eq!(parse_playlist(""), Err(PlaylistError::Empty));
    }

    #[test]
    fn relative_entries_are_kept_for_the_caller_to_resolve() {
        let entries = parse_playlist("#EXTM3U\nlive.mp3\n").unwrap();
        assert_eq!(urls(&entries), ["live.mp3"]);
        assert!(!is_http_url(&entries[0].url));
    }

    #[test]
    fn only_http_and_https_are_stations() {
        assert!(is_http_url("HTTP://a.example/x"));
        assert!(is_http_url(" https://a.example/x"));
        assert!(!is_http_url("file:///etc/passwd"));
        assert!(!is_http_url("rtsp://a.example/x"));
        assert!(!is_http_url("C:\\music\\a.mp3"));
    }

    #[test]
    fn playlist_extensions_are_matched_without_case() {
        assert!(has_playlist_extension("/listen.PLS"));
        assert!(has_playlist_extension("live.m3u8"));
        assert!(!has_playlist_extension("/stream.mp3"));
    }
}

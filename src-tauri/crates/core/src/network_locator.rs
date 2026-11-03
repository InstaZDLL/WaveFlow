//! The `file_path` of a track that lives on a WebDAV or SMB share.
//!
//! `wfnet://<source uuid>/<path on the share>`, with the path as the server
//! names it and **not percent-encoded**: `file_stem`, the folder tree and
//! every display of the path stay readable, and only the WebDAV client
//! encodes, one segment at a time, when it builds a request.
//!
//! It is a locator rather than a URL on purpose: nothing may hand it to a URL
//! parser (a `#` in a file name is an ordinary character here) and it carries
//! no host and no credential. The uuid is the only link to the
//! `network_source` row, so moving a NAS to another address rewrites nothing.
//!
//! A locator survives a `PathBuf` round trip on every platform as long as
//! nothing joins or canonicalizes it, which is why code that opens files asks
//! [`is_locator_path`] first.

use std::path::Path;

/// What every locator starts with.
pub const PREFIX: &str = "wfnet://";

/// Extensions a share never offers, among the scanner's audio extensions:
/// the DSD pipeline opens a `File`, and these files run to gigabytes.
pub const LOCAL_ONLY_EXTENSIONS: &[&str] = &["dsf", "dff"];

/// A locator taken apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Locator<'a> {
    pub source: &'a str,
    /// Relative to the share's root, `/`-separated, no leading `/`. Empty for
    /// the root itself.
    pub path: &'a str,
}

/// Whether a stored path is a locator rather than a path on this machine.
pub fn is_locator(path: &str) -> bool {
    path.starts_with(PREFIX)
}

/// [`is_locator`] for a [`Path`], the shape most call sites hold.
pub fn is_locator_path(path: &Path) -> bool {
    path.to_str().is_some_and(is_locator)
}

/// Take a locator apart. `None` for anything else, for a locator that names no
/// source, or for one [`build`] would have refused: an imported profile can hold
/// any string, and what comes out of here goes to a client with a password.
pub fn parse(locator: &str) -> Option<Locator<'_>> {
    let rest = locator.strip_prefix(PREFIX)?;
    let (source, path) = rest.split_once('/').unwrap_or((rest, ""));
    if !is_safe_source(source) || !is_safe_path(path) {
        return None;
    }
    Some(Locator { source, path })
}

/// Build a locator, refusing a path that climbs out of the share.
///
/// A name comes from the server, so it is data: `..` would walk out of the
/// folder the user chose, and a stored locator is later handed to a client
/// holding their password.
pub fn build(source: &str, path: &str) -> Option<String> {
    let path = path.trim_matches('/');
    if !is_safe_source(source) || !is_safe_path(path) {
        return None;
    }
    Some(if path.is_empty() {
        format!("{PREFIX}{source}")
    } else {
        format!("{PREFIX}{source}/{path}")
    })
}

/// Whether a source reads like the identifier it is. Its first characters name
/// a file in the audio cache, so a relative path in a stored locator would write
/// outside it.
fn is_safe_source(source: &str) -> bool {
    !source.is_empty()
        && source
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Whether a share-relative path stays inside its root: no empty, `.` or `..`
/// segment, and no backslash, which SMB and Windows servers read as a
/// separator.
pub fn is_safe_path(path: &str) -> bool {
    path.is_empty()
        || path
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | "..") && !segment.contains('\\'))
}

/// The file name a locator ends with, as the server spells it.
pub fn file_name(locator: &str) -> Option<&str> {
    let parsed = parse(locator)?;
    let name = parsed.path.rsplit('/').next()?;
    (!name.is_empty()).then_some(name)
}

/// The directory a share-relative path lies in.
pub fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locator_round_trips_with_the_characters_a_url_would_eat() {
        let locator = build("abc", "Rock/AC#DC ? 100%.flac").unwrap();
        assert_eq!(locator, "wfnet://abc/Rock/AC#DC ? 100%.flac");
        let parsed = parse(&locator).unwrap();
        assert_eq!(parsed.source, "abc");
        assert_eq!(parsed.path, "Rock/AC#DC ? 100%.flac");
        assert_eq!(file_name(&locator), Some("AC#DC ? 100%.flac"));
        assert_eq!(parent_dir(parsed.path), "Rock");
    }

    #[test]
    fn a_path_cannot_climb_out_of_the_share() {
        assert_eq!(build("abc", "Rock/../../etc/passwd"), None);
        assert_eq!(build("abc", "./a.flac"), None);
        assert_eq!(build("abc", "a//b.flac"), None);
        assert_eq!(build("abc", r"Rock\..\b.flac"), None);
        assert_eq!(build("", "a.flac"), None);
    }

    #[test]
    fn the_root_of_a_share_has_a_locator_of_its_own() {
        assert_eq!(build("abc", "").as_deref(), Some("wfnet://abc"));
        let parsed = parse("wfnet://abc").unwrap();
        assert_eq!(parsed.path, "");
        assert_eq!(file_name("wfnet://abc"), None);
    }

    #[test]
    fn a_local_path_is_not_a_locator() {
        for path in [
            "/music/a.flac",
            r"C:\music\a.flac",
            r"\\nas\music\a.flac",
            "wfnet:/a",
        ] {
            assert!(!is_locator(path), "{path}");
            assert_eq!(parse(path), None, "{path}");
        }
        assert_eq!(parse("wfnet://"), None);
    }

    #[test]
    fn a_source_is_an_identifier_and_nothing_else() {
        for source in [r"..\..\x", "..", "C:", "a.b", "a b", "é"] {
            assert_eq!(build(source, "a.flac"), None, "{source}");
            let locator = format!("wfnet://{source}/a.flac");
            assert_eq!(parse(&locator), None, "{source}");
        }
        assert!(build("11111111-aaaa", "a.flac").is_some());
    }

    #[test]
    fn a_stored_locator_that_climbs_out_of_the_share_does_not_parse() {
        for locator in [
            "wfnet://abc/../secret.flac",
            "wfnet://abc/Music/../../secret.flac",
            "wfnet://abc/Music/./a.flac",
            r"wfnet://abc/Music\..\a.flac",
            "wfnet://abc//a.flac",
        ] {
            assert_eq!(parse(locator), None, "{locator}");
        }
    }

    #[test]
    fn a_locator_survives_a_path_buffer() {
        let locator = "wfnet://abc/Rock/AC#DC.flac";
        let path = std::path::PathBuf::from(locator);
        assert!(is_locator_path(&path));
        assert_eq!(path.to_str(), Some(locator));
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("flac"));
        assert_eq!(path.file_stem().and_then(|e| e.to_str()), Some("AC#DC"));
    }
}

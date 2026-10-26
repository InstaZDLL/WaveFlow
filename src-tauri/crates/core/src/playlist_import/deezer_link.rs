//! What the user pastes to import a Deezer playlist.
//!
//! A public playlist is readable through the unauthenticated API by its
//! id, so the link only has to give the id up: a full
//! `https://www.deezer.com/<lang>/playlist/<id>` link, the same without the
//! language or the scheme, or the bare number. A share link
//! (`link.deezer.com/…`) names no id; it redirects to a full link, which
//! the caller follows — on Deezer's hosts only — and reads here again.

/// The hosts a share link may redirect through. Anything else is not
/// Deezer, and is not followed.
pub const SHARE_HOSTS: [&str; 2] = ["link.deezer.com", "deezer.page.link"];

/// Hosts a full playlist link lives on.
const SITE_HOSTS: [&str; 2] = ["www.deezer.com", "deezer.com"];

/// What a pasted string is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeezerLink {
    /// A playlist id, ready for the API.
    Playlist(u64),
    /// A share link: follow its redirect, then parse where it leads.
    Share(String),
}

/// Split `scheme://host/path?query#frag` into a lowercase host and the
/// path, the scheme being optional. No URL crate is needed for links this
/// regular, and none is in this crate.
fn host_and_path(raw: &str) -> Option<(String, String)> {
    let rest = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))
        .unwrap_or(raw);
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    // A userinfo or a port has no business in a playlist link.
    if host.contains('@') || host.contains(':') || host.is_empty() {
        return None;
    }
    Some((host.to_ascii_lowercase(), path.to_string()))
}

/// Read what the user pasted. `None` when it is no Deezer playlist link.
pub fn parse(input: &str) -> Option<DeezerLink> {
    let input = input.trim();
    if !input.is_empty() && input.len() <= 20 && input.bytes().all(|b| b.is_ascii_digit()) {
        return input
            .parse()
            .ok()
            .filter(|id| *id > 0)
            .map(DeezerLink::Playlist);
    }
    let (host, path) = host_and_path(input)?;
    if SHARE_HOSTS.contains(&host.as_str()) {
        return (!path.is_empty()).then(|| DeezerLink::Share(format!("https://{host}/{path}")));
    }
    if !SITE_HOSTS.contains(&host.as_str()) {
        return None;
    }
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // `/playlist/<id>` or `/<lang>/playlist/<id>`.
    let id = match segments.as_slice() {
        ["playlist", id, ..] => id,
        [_, "playlist", id, ..] => id,
        _ => return None,
    };
    id.parse()
        .ok()
        .filter(|id| *id > 0)
        .map(DeezerLink::Playlist)
}

/// Whether a redirect target is still on Deezer — the only place a share
/// link is followed to.
pub fn is_deezer_host(url: &str) -> bool {
    host_and_path(url).is_some_and(|(host, _)| {
        SHARE_HOSTS.contains(&host.as_str()) || SITE_HOSTS.contains(&host.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_links_and_bare_ids_give_the_playlist_id() {
        for input in [
            "https://www.deezer.com/fr/playlist/1109890291",
            "https://www.deezer.com/playlist/1109890291?utm_source=x",
            "deezer.com/en/playlist/1109890291/",
            "  1109890291 ",
            "http://WWW.DEEZER.COM/playlist/1109890291#top",
        ] {
            assert_eq!(
                parse(input),
                Some(DeezerLink::Playlist(1_109_890_291)),
                "{input}"
            );
        }
    }

    #[test]
    fn share_links_are_followed_not_read() {
        assert_eq!(
            parse("https://link.deezer.com/s/30ABCdef"),
            Some(DeezerLink::Share(
                "https://link.deezer.com/s/30ABCdef".into()
            ))
        );
        assert_eq!(parse("https://link.deezer.com/"), None);
    }

    #[test]
    fn anything_else_is_refused() {
        for input in [
            "",
            "https://www.deezer.com/fr/album/302127",
            "https://evil.example/playlist/1",
            "https://www.deezer.com.evil.example/playlist/1",
            "https://user@www.deezer.com/playlist/1",
            "https://www.deezer.com:8443/playlist/1",
            "https://www.deezer.com/playlist/abc",
            "https://www.deezer.com/playlist/0",
        ] {
            assert_eq!(parse(input), None, "{input}");
        }
        assert!(is_deezer_host("https://www.deezer.com/fr/playlist/1"));
        assert!(!is_deezer_host("https://deezer.com.evil.example/"));
    }
}

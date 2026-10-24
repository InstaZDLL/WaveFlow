//! Adding a Web Radio station the directory does not list.
//!
//! The Web Radio view browses radio-browser (or its offline catalogue),
//! and a station missing from it had no way in. The user pastes the
//! stream's URL — or the `.m3u` / `.pls` its website links to — or opens
//! such a file, and the station joins their favourites: a favourite is
//! already a station by URL (`url:<stream>`, the id the plugin resolves
//! without the network), so a station added here plays, is renamed and
//! is removed exactly like one starred from the directory, with no table
//! of its own.
//!
//! What this module does is make sure the URL is a station before it is
//! kept: [`web_radio_probe_url`] opens it once, short, reads the headers,
//! follows one playlist to the stream it names, and answers with the
//! stream URL and the name the server announces (`icy-name`).
//!
//! Three rules ride along:
//!
//! - **`http` / `https` only**, the two schemes the engine streams.
//! - **Offline mode is honoured** before a socket is opened.
//! - **No secret reaches the log or an error.** A station URL may carry
//!   credentials (`user:pass@`, `?key=`): every URL logged goes through
//!   [`redact_url`], and a `reqwest::Error` through `without_url()` —
//!   the error strings are shown in the dialog, and the dialog is
//!   screenshotted into bug reports.

use std::time::Duration;

use reqwest::header::{HeaderMap, CONTENT_TYPE};
use serde::Serialize;
use waveflow_core::radio_playlist::{
    has_playlist_extension, is_http_url, parse_playlist, PlaylistEntry, PlaylistError,
};

use crate::audio::http_source::redact_url;
use crate::error::{AppError, AppResult};

/// How long a station gets to accept the connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

/// How long it gets to answer with its headers. Only the headers: a live
/// stream's body never ends, so no deadline may cover it, and nothing
/// here reads it.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// The most of a playlist read over the network. A station playlist is a
/// few lines; anything bigger is not one, and the first entries — the
/// only ones used — are at the top.
const MAX_PLAYLIST_BYTES: usize = 64 * 1024;

/// The largest playlist file opened from disk.
const MAX_PLAYLIST_FILE_BYTES: u64 = 1024 * 1024;

const USER_AGENT: &str = concat!("WaveFlow/", env!("CARGO_PKG_VERSION"));

/// The longest station name kept from a header or a playlist.
const MAX_NAME_CHARS: usize = 200;

// The refusals the add-station dialog recognises and translates. Each
// error starts with one of these; what follows is detail for the log.
pub const ERR_NOT_HTTP: &str = "radio: not an http(s) url";
pub const ERR_OFFLINE: &str = "radio: offline";
pub const ERR_UNREACHABLE: &str = "radio: unreachable";
pub const ERR_NOT_AUDIO: &str = "radio: not an audio stream";
pub const ERR_HLS: &str = "radio: hls";
pub const ERR_NO_STREAM: &str = "radio: no stream";

/// A URL that answered as a station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbedStation {
    /// What plays: the URL given when it was the stream, the stream a
    /// playlist named otherwise.
    pub stream_url: String,
    /// `icy-name`, or the playlist's own title for the stream.
    pub name: Option<String>,
}

/// Open `url` once and answer with the station behind it.
#[tauri::command]
pub async fn web_radio_probe_url(url: String) -> AppResult<ProbedStation> {
    probe(&url).await
}

/// The streams a `.m3u` / `.m3u8` / `.pls` file names, absolute
/// `http(s)` ones only — a file on disk has no URL to resolve a relative
/// entry against. The dialog probes the first through
/// [`web_radio_probe_url`], like a pasted URL.
#[tauri::command]
pub async fn web_radio_read_playlist_file(path: String) -> AppResult<Vec<PlaylistEntry>> {
    tokio::task::spawn_blocking(move || read_playlist_file(std::path::Path::new(&path)))
        .await
        .map_err(|e| AppError::Other(format!("playlist read join: {e}")))?
}

fn read_playlist_file(path: &std::path::Path) -> AppResult<Vec<PlaylistEntry>> {
    let len = std::fs::metadata(path)?.len();
    if len > MAX_PLAYLIST_FILE_BYTES {
        return Err(AppError::Other(format!(
            "{ERR_NO_STREAM}: the file is too large to be a station playlist ({len} bytes)"
        )));
    }
    let bytes = std::fs::read(path)?;
    let entries = parse_playlist(&String::from_utf8_lossy(&bytes)).map_err(playlist_error)?;
    let entries: Vec<PlaylistEntry> = entries
        .into_iter()
        .filter(|e| is_http_url(&e.url))
        .map(|e| PlaylistEntry {
            url: e.url.trim().to_string(),
            title: e.title.map(clip_name),
        })
        .collect();
    if entries.is_empty() {
        return Err(AppError::Other(format!(
            "{ERR_NO_STREAM}: the playlist names no http(s) stream"
        )));
    }
    Ok(entries)
}

pub(crate) async fn probe(raw: &str) -> AppResult<ProbedStation> {
    let url = parse_station_url(raw)?;
    if crate::offline::is_offline() {
        return Err(AppError::Other(ERR_OFFLINE.into()));
    }
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|e| AppError::Other(format!("http client build: {}", e.without_url())))?;

    let response = open(&client, &url).await?;
    let station = match classify(response.headers(), response.url().path()) {
        Kind::Audio => ProbedStation {
            stream_url: url.to_string(),
            name: icy_name(response.headers()),
        },
        Kind::Playlist => {
            let base = response.url().clone();
            let body = read_capped(response).await?;
            let entries = parse_playlist(&body).map_err(playlist_error)?;
            // The first entry that is a station: a `.pls` lists mirrors
            // of one stream, and the first is the one its author meant.
            let (entry, stream) = entries
                .iter()
                .find_map(|e| {
                    let resolved = base.join(e.url.trim()).ok()?;
                    matches!(resolved.scheme(), "http" | "https").then_some((e, resolved))
                })
                .ok_or_else(|| {
                    AppError::Other(format!(
                        "{ERR_NO_STREAM}: the playlist names no http(s) stream"
                    ))
                })?;
            // One level only: a playlist naming another playlist is not
            // followed, so a loop of them cannot keep this busy.
            let inner = open(&client, &stream).await?;
            match classify(inner.headers(), inner.url().path()) {
                Kind::Audio => ProbedStation {
                    stream_url: stream.to_string(),
                    name: entry
                        .title
                        .clone()
                        .map(clip_name)
                        .or_else(|| icy_name(inner.headers())),
                },
                Kind::Playlist => {
                    return Err(AppError::Other(format!(
                        "{ERR_NOT_AUDIO}: the playlist names another playlist"
                    )))
                }
                Kind::Other(ct) => {
                    return Err(AppError::Other(format!("{ERR_NOT_AUDIO}: {ct}")));
                }
            }
        }
        Kind::Other(ct) => return Err(AppError::Other(format!("{ERR_NOT_AUDIO}: {ct}"))),
    };
    tracing::info!(
        url = %redact_url(&station.stream_url),
        "web radio station probed"
    );
    Ok(station)
}

fn parse_station_url(raw: &str) -> AppResult<url::Url> {
    let url =
        url::Url::parse(raw.trim()).map_err(|e| AppError::Other(format!("{ERR_NOT_HTTP}: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(AppError::Other(format!("{ERR_NOT_HTTP}: {}", url.scheme())));
    }
    Ok(url)
}

/// GET `url` and wait for its headers, not its body.
async fn open(client: &reqwest::Client, url: &url::Url) -> AppResult<reqwest::Response> {
    let send = client
        .get(url.clone())
        // Asked for so a server that only names itself to a player that
        // wants metadata does so here too; the body is never read.
        .header("Icy-MetaData", "1")
        .send();
    let response = match tokio::time::timeout(RESPONSE_TIMEOUT, send).await {
        Ok(Ok(response)) => response,
        Ok(Err(err)) => {
            let err = err.without_url();
            tracing::info!(url = %redact_url(url.as_str()), error = %err, "web radio probe failed");
            return Err(AppError::Other(format!("{ERR_UNREACHABLE}: {err}")));
        }
        Err(_) => {
            tracing::info!(url = %redact_url(url.as_str()), "web radio probe timed out");
            return Err(AppError::Other(format!("{ERR_UNREACHABLE}: timed out")));
        }
    };
    if !response.status().is_success() {
        tracing::info!(
            url = %redact_url(url.as_str()),
            status = %response.status(),
            "web radio probe answered with an error status"
        );
        return Err(AppError::Other(format!(
            "{ERR_UNREACHABLE}: {}",
            response.status()
        )));
    }
    Ok(response)
}

async fn read_capped(mut response: reqwest::Response) -> AppResult<String> {
    let mut bytes: Vec<u8> = Vec::new();
    let read = async {
        while let Some(chunk) = response.chunk().await? {
            let room = MAX_PLAYLIST_BYTES - bytes.len();
            bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
            if bytes.len() >= MAX_PLAYLIST_BYTES {
                break;
            }
        }
        Ok::<(), reqwest::Error>(())
    };
    match tokio::time::timeout(RESPONSE_TIMEOUT, read).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            return Err(AppError::Other(format!(
                "{ERR_UNREACHABLE}: {}",
                err.without_url()
            )))
        }
        Err(_) => return Err(AppError::Other(format!("{ERR_UNREACHABLE}: timed out"))),
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// What a response is, by its `Content-Type` — and by the path's
/// extension where the type says nothing useful.
#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Audio,
    Playlist,
    Other(String),
}

fn classify(headers: &HeaderMap, path: &str) -> Kind {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    match content_type.as_str() {
        "audio/x-mpegurl"
        | "audio/mpegurl"
        | "application/x-mpegurl"
        | "application/vnd.apple.mpegurl"
        | "audio/x-scpls"
        | "audio/scpls"
        | "application/pls"
        | "application/pls+xml" => Kind::Playlist,
        ct if ct.starts_with("audio/") || ct == "application/ogg" || ct == "video/ogg" => {
            Kind::Audio
        }
        // Says nothing: trust the extension, and take a typeless or
        // binary answer for the stream itself.
        "" | "application/octet-stream" | "binary/octet-stream" => {
            if has_playlist_extension(path) {
                Kind::Playlist
            } else {
                Kind::Audio
            }
        }
        "text/plain" if has_playlist_extension(path) => Kind::Playlist,
        other => Kind::Other(other.to_string()),
    }
}

fn icy_name(headers: &HeaderMap) -> Option<String> {
    let name = headers.get("icy-name")?.to_str().ok()?.trim();
    (!name.is_empty()).then(|| clip_name(name.to_string()))
}

fn clip_name(name: String) -> String {
    match name.char_indices().nth(MAX_NAME_CHARS) {
        Some((end, _)) => name[..end].to_string(),
        None => name,
    }
}

fn playlist_error(err: PlaylistError) -> AppError {
    match err {
        PlaylistError::Hls => AppError::Other(format!("{ERR_HLS}: {err}")),
        PlaylistError::Empty => AppError::Other(format!("{ERR_NO_STREAM}: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A one-file HTTP server: each request gets the canned answer for
    /// its path, and the listener runs until the test ends.
    async fn serve(routes: Vec<(&'static str, String)>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let routes = routes.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let path = request
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("/")
                        .split('?')
                        .next()
                        .unwrap_or("/")
                        .to_string();
                    let answer = routes
                        .iter()
                        .find(|(p, _)| *p == path)
                        .map(|(_, a)| a.clone())
                        .unwrap_or_else(|| {
                            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string()
                        });
                    let _ = socket.write_all(answer.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        format!("http://{addr}")
    }

    fn audio(content_type: &str, name: Option<&str>) -> String {
        let icy = name
            .map(|n| format!("icy-name: {n}\r\n"))
            .unwrap_or_default();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n{icy}Content-Length: 4\r\n\r\nID3\0"
        )
    }

    fn text(content_type: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn a_stream_answers_with_its_own_url_and_name() {
        let base = serve(vec![("/live", audio("audio/mpeg", Some("Test FM")))]).await;
        let station = probe(&format!("{base}/live")).await.unwrap();
        assert_eq!(station.stream_url, format!("{base}/live"));
        assert_eq!(station.name.as_deref(), Some("Test FM"));
    }

    #[tokio::test]
    async fn a_playlist_is_followed_to_the_stream_it_names() {
        let pls = "[playlist]\nFile1=/live\nTitle1=Jazz Club\nNumberOfEntries=1\n";
        let m3u = "#EXTM3U\n#EXTINF:-1,Relative One\nlive\n";
        let base = serve(vec![
            ("/listen.pls", text("audio/x-scpls", pls)),
            ("/dir/list.m3u", text("text/plain", m3u)),
            ("/live", audio("audio/aac", Some("From The Server"))),
            ("/dir/live", audio("audio/ogg", None)),
        ])
        .await;
        let station = probe(&format!("{base}/listen.pls")).await.unwrap();
        assert_eq!(station.stream_url, format!("{base}/live"));
        assert_eq!(station.name.as_deref(), Some("Jazz Club"));

        // A relative entry resolves against the playlist's own URL.
        let station = probe(&format!("{base}/dir/list.m3u")).await.unwrap();
        assert_eq!(station.stream_url, format!("{base}/dir/live"));
        assert_eq!(station.name.as_deref(), Some("Relative One"));
    }

    #[tokio::test]
    async fn what_is_not_a_station_is_refused_with_its_reason() {
        let hls = "#EXTM3U\n#EXT-X-VERSION:3\n#EXTINF:6,\nseg.ts\n";
        let base = serve(vec![
            ("/page", text("text/html", "<html></html>")),
            ("/hls.m3u8", text("application/vnd.apple.mpegurl", hls)),
            ("/empty.m3u", text("audio/x-mpegurl", "#EXTM3U\n")),
            ("/nested.m3u", text("audio/x-mpegurl", "/empty.m3u\n")),
        ])
        .await;
        let err = |r: AppResult<ProbedStation>| r.unwrap_err().to_string();
        assert!(err(probe(&format!("{base}/page")).await).contains(ERR_NOT_AUDIO));
        assert!(err(probe(&format!("{base}/hls.m3u8")).await).contains(ERR_HLS));
        assert!(err(probe(&format!("{base}/empty.m3u")).await).contains(ERR_NO_STREAM));
        assert!(err(probe(&format!("{base}/nested.m3u")).await).contains(ERR_NOT_AUDIO));
        assert!(err(probe(&format!("{base}/missing")).await).contains(ERR_UNREACHABLE));
        assert!(err(probe("ftp://example.com/live").await).contains(ERR_NOT_HTTP));
        assert!(err(probe("file:///etc/passwd").await).contains(ERR_NOT_HTTP));
        assert!(err(probe("not a url").await).contains(ERR_NOT_HTTP));
    }

    /// The error goes to the dialog and the log: credentials in the URL
    /// must reach neither.
    #[tokio::test]
    async fn an_error_never_carries_the_credentials() {
        // A port nothing listens on: bind, read the port, drop.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let err = probe(&format!(
            "http://alice:hunter2@127.0.0.1:{port}/live?key=s3cret"
        ))
        .await
        .unwrap_err()
        .to_string();
        assert!(err.starts_with(ERR_UNREACHABLE), "{err}");
        assert!(!err.contains("hunter2") && !err.contains("s3cret"), "{err}");
    }

    #[test]
    fn a_playlist_file_keeps_its_absolute_streams() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("station.pls");
        std::fs::write(
            &path,
            "[playlist]\nFile1=relative.mp3\nFile2=https://a.example/live\nTitle2=A\n",
        )
        .unwrap();
        let entries = read_playlist_file(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].url, "https://a.example/live");
        assert_eq!(entries[0].title.as_deref(), Some("A"));

        std::fs::write(&path, "#EXTM3U\nrelative.mp3\n").unwrap();
        let err = read_playlist_file(&path).unwrap_err().to_string();
        assert!(err.contains(ERR_NO_STREAM), "{err}");
    }
}

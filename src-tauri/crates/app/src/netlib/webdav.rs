//! A WebDAV share, over the HTTP client and the XML parser the app already
//! carries.
//!
//! Only `PROPFIND` with a depth of 0 or 1 (`infinity` is often refused, and
//! would make one enormous answer of a listing that can otherwise report as
//! it goes) and `GET` with a `Range`.
//!
//! ## Authentication and TLS
//!
//! Basic only. The certificate is always checked and there is no way to turn
//! that off: a self-signed NAS is refused with the cause named. `http://`
//! needs the user's explicit consent, recorded on the source, because Basic
//! then crosses the network in clear; and a redirect from `https` to `http`
//! is never followed, since it would take that decision away from them.
//!
//! ## What the server says is data
//!
//! A `href` only ever contributes its last segment, as a name, and a name
//! that could climb out of a directory is dropped. The URL of a request is
//! built from our base and the path, one encoded segment at a time.

use std::time::Duration;

use futures::future::BoxFuture;
use quick_xml::events::Event;
use reqwest::{header, Method, StatusCode};
use url::Url;
use waveflow_core::error::describe_http_error;
use waveflow_core::network_locator::is_safe_path;

use super::fs::{Entry, NetError, NetworkFs};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// What a directory listing may weigh. A few hundred bytes per entry, so far
/// beyond any real directory; a server sending more is not a share.
const MAX_LISTING_BYTES: usize = 64 * 1024 * 1024;

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:"><D:prop><D:resourcetype/><D:getcontentlength/><D:getlastmodified/></D:prop></D:propfind>"#;

pub struct WebDavFs {
    client: reqwest::Client,
    base: Url,
    username: Option<String>,
    password: Option<String>,
}

/// Why an endpoint is refused before any request is made.
pub fn parse_endpoint(endpoint: &str, allow_plain_http: bool) -> Result<Url, NetError> {
    let mut base = Url::parse(endpoint.trim())
        .map_err(|_| NetError::Protocol("not a valid address".into()))?;
    match base.scheme() {
        "https" => {}
        "http" if allow_plain_http => {}
        "http" => return Err(NetError::Protocol("plain-http-not-accepted".into())),
        _ => return Err(NetError::Protocol("not an http(s) address".into())),
    }
    if base.host_str().is_none() {
        return Err(NetError::Protocol("not a valid address".into()));
    }
    // The credentials have their own fields. In the address they would end up
    // in the stored endpoint, and from there in a backup or a log.
    if !base.username().is_empty() || base.password().is_some() {
        return Err(NetError::Protocol(
            "the address must not contain credentials".into(),
        ));
    }
    base.set_query(None);
    base.set_fragment(None);
    let trimmed = base.path().trim_end_matches('/').to_string();
    base.set_path(&trimmed);
    Ok(base)
}

impl WebDavFs {
    pub fn new(
        endpoint: &str,
        username: Option<String>,
        password: Option<String>,
        allow_plain_http: bool,
    ) -> Result<Self, NetError> {
        let base = parse_endpoint(endpoint, allow_plain_http)?;
        let client = reqwest::Client::builder()
            .user_agent(crate::audio::http_source::USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let downgrade = attempt.url().scheme() == "http"
                    && attempt.previous().iter().any(|u| u.scheme() == "https");
                if downgrade || attempt.previous().len() >= 5 {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|e| NetError::Protocol(describe_http_error(&e)))?;
        Ok(Self {
            client,
            base,
            username: username.filter(|u| !u.is_empty()),
            password,
        })
    }

    fn url_for(&self, path: &str, as_dir: bool) -> Result<Url, NetError> {
        let mut url = self.base.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| NetError::Protocol("not a valid address".into()))?;
            segments.pop_if_empty();
            for segment in path.split('/').filter(|s| !s.is_empty()) {
                segments.push(segment);
            }
            if as_dir {
                segments.push("");
            }
        }
        Ok(url)
    }

    fn request(&self, method: Method, url: Url) -> reqwest::RequestBuilder {
        let request = self.client.request(method, url);
        match &self.username {
            Some(username) => request.basic_auth(username, self.password.as_deref()),
            None => request,
        }
    }

    async fn propfind(&self, path: &str, depth: &str) -> Result<Vec<RawEntry>, NetError> {
        let method = Method::from_bytes(b"PROPFIND")
            .map_err(|_| NetError::Protocol("PROPFIND".to_string()))?;
        let url = self.url_for(path, depth == "1")?;
        let mut response = self
            .request(method, url.clone())
            .header("Depth", depth)
            .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
            .body(PROPFIND_BODY)
            .send()
            .await
            .map_err(classify_transport)?;
        let status = response.status();
        if status != StatusCode::MULTI_STATUS {
            return Err(classify_status(status, response.headers()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(classify_transport)? {
            if body.len() + chunk.len() > MAX_LISTING_BYTES {
                return Err(NetError::Protocol("listing too large".into()));
            }
            body.extend_from_slice(&chunk);
        }
        parse_multistatus(&body).map_err(NetError::Protocol)
    }
}

fn classify_transport(err: reqwest::Error) -> NetError {
    let detail = describe_http_error(&err);
    let lower = detail.to_ascii_lowercase();
    if lower.contains("certificate") {
        NetError::Tls(detail)
    } else {
        NetError::Unreachable(detail)
    }
}

fn classify_status(status: StatusCode, headers: &header::HeaderMap) -> NetError {
    match status {
        StatusCode::UNAUTHORIZED => {
            // Say which scheme it wanted when it is one we do not speak, so
            // "wrong password" is not the only conclusion left to the user.
            let digest_only = headers
                .get(header::WWW_AUTHENTICATE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| {
                    let v = v.to_ascii_lowercase();
                    v.contains("digest") && !v.contains("basic")
                });
            NetError::Auth(if digest_only {
                "the server only accepts Digest authentication".into()
            } else {
                "wrong user name or password".into()
            })
        }
        StatusCode::FORBIDDEN => NetError::Auth("this account has no access here".into()),
        StatusCode::NOT_FOUND | StatusCode::GONE => NetError::NotFound,
        StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED | StatusCode::OK => {
            NetError::Protocol("this address does not speak WebDAV".into())
        }
        status if status.is_server_error() => {
            NetError::Unreachable(format!("server error {}", status.as_u16()))
        }
        status => NetError::Protocol(format!("unexpected answer {}", status.as_u16())),
    }
}

impl NetworkFs for WebDavFs {
    fn probe(&self) -> BoxFuture<'_, Result<(), NetError>> {
        Box::pin(async move { self.propfind("", "0").await.map(|_| ()) })
    }

    fn list<'a>(&'a self, dir: &'a str) -> BoxFuture<'a, Result<Vec<Entry>, NetError>> {
        Box::pin(async move {
            let raw = self.propfind(dir, "1").await?;
            let wanted = self.url_for(dir, false)?;
            let wanted: Vec<String> = decoded_segments(wanted.path());
            Ok(children(raw, &wanted))
        })
    }

    fn stat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Entry, NetError>> {
        Box::pin(async move {
            let raw = self.propfind(path, "0").await?;
            let first = raw
                .into_iter()
                .next()
                .ok_or_else(|| NetError::Protocol("empty answer".into()))?;
            Ok(Entry {
                name: path.rsplit('/').next().unwrap_or("").to_string(),
                is_dir: first.is_dir,
                size: first.size,
                modified_ms: first.modified_ms,
            })
        })
    }

    fn read_at<'a>(
        &'a self,
        path: &'a str,
        offset: u64,
        len: u64,
    ) -> BoxFuture<'a, Result<Vec<u8>, NetError>> {
        Box::pin(async move {
            if len == 0 {
                return Ok(Vec::new());
            }
            let url = self.url_for(path, false)?;
            let mut response = self
                .request(Method::GET, url)
                .header(header::ACCEPT, "*/*")
                .header(
                    header::RANGE,
                    format!("bytes={offset}-{}", offset + len - 1),
                )
                .send()
                .await
                .map_err(classify_transport)?;
            let status = response.status();
            let whole_file = status == StatusCode::OK;
            if status == StatusCode::RANGE_NOT_SATISFIABLE {
                // Asked at or past the end, which is where a reader lands to
                // discover there is nothing left.
                return Ok(Vec::new());
            }
            if status != StatusCode::PARTIAL_CONTENT && !whole_file {
                return Err(classify_status(status, response.headers()));
            }
            if whole_file && offset > 0 {
                // It ignored the range and is sending from byte 0: playing
                // that as the bytes we asked for would be noise.
                return Err(NetError::Protocol(
                    "this server does not support byte ranges".into(),
                ));
            }
            if !whole_file {
                let start = response
                    .headers()
                    .get(header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(crate::audio::http_source::parse_content_range_start);
                if start != Some(offset) {
                    return Err(NetError::Protocol("wrong byte range in the answer".into()));
                }
            }
            let mut out = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
            while let Some(chunk) = response.chunk().await.map_err(classify_transport)? {
                let room = usize::try_from(len).unwrap_or(usize::MAX) - out.len();
                out.extend_from_slice(&chunk[..chunk.len().min(room)]);
                if out.len() as u64 >= len {
                    break;
                }
            }
            Ok(out)
        })
    }
}

/// One `<response>` of a `207`, the `href` still as the server spelled it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct RawEntry {
    pub href: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_ms: i64,
}

/// Split a URL path into its segments, percent-decoded.
fn decoded_segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(percent_decode)
        .collect()
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The path part of a `href`, which may be a whole URL.
fn href_path(href: &str) -> &str {
    let without_origin = match href.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("", |at| &rest[at..]),
        None => href,
    };
    without_origin
        .split(['?', '#'])
        .next()
        .unwrap_or(without_origin)
}

/// The entries of a `Depth: 1` answer below the directory `wanted`.
///
/// A name is the last segment of its `href`, and nothing else from it is used.
/// The directory's own entry is told by its full path, or, behind a reverse
/// proxy that rewrites the prefix, by being the first directory answered.
fn children(raw: Vec<RawEntry>, wanted: &[String]) -> Vec<Entry> {
    let mut out = Vec::with_capacity(raw.len());
    for (index, entry) in raw.into_iter().enumerate() {
        let segments = decoded_segments(href_path(&entry.href));
        if segments.as_slice() == wanted {
            continue;
        }
        let under_wanted = segments.len() == wanted.len() + 1 && segments.starts_with(wanted);
        if index == 0 && entry.is_dir && !under_wanted {
            continue;
        }
        let Some(name) = segments.last() else {
            continue;
        };
        // One segment, and not one that climbs.
        if !is_safe_path(name) || name.contains('/') {
            tracing::warn!("network listing: dropped an entry with an unsafe name");
            continue;
        }
        out.push(Entry {
            name: name.clone(),
            is_dir: entry.is_dir,
            size: if entry.is_dir { 0 } else { entry.size },
            modified_ms: entry.modified_ms,
        });
    }
    out
}

/// Read a `207 Multi-Status`. Element names are compared without their prefix
/// (servers use `D:`, `d:` or none at all), and a property counts only from a
/// `propstat` the server marked as found.
pub(super) fn parse_multistatus(xml: &[u8]) -> Result<Vec<RawEntry>, String> {
    let xml = String::from_utf8_lossy(xml);
    let mut reader = quick_xml::Reader::from_str(&xml);
    let mut entries = Vec::new();
    let mut current: Option<RawEntry> = None;
    let mut found = RawEntry::default();
    let mut propstat_ok = true;
    let mut in_resourcetype = false;
    let mut text = String::new();
    // Whether this response had a propstat the server marked as found.
    let mut any_found = false;
    let mut closed = false;
    loop {
        let event = reader
            .read_event()
            .map_err(|e| format!("invalid XML: {e}"))?;
        match event {
            Event::Eof => break,
            Event::Start(element) => {
                text.clear();
                match element.local_name().as_ref() {
                    "response" => {
                        current = Some(RawEntry::default());
                        any_found = false;
                    }
                    "propstat" => {
                        found = RawEntry::default();
                        propstat_ok = true;
                    }
                    "resourcetype" => in_resourcetype = true,
                    "collection" if in_resourcetype => found.is_dir = true,
                    _ => {}
                }
            }
            Event::Empty(element) => {
                if in_resourcetype && element.local_name().as_ref() == "collection" {
                    found.is_dir = true;
                }
            }
            Event::Text(value) => text.push_str(&value.xml10_content()),
            Event::CData(value) => text.push_str(&value.xml10_content()),
            Event::GeneralRef(reference) => match &*reference {
                "amp" => text.push('&'),
                "lt" => text.push('<'),
                "gt" => text.push('>'),
                "quot" => text.push('"'),
                "apos" => text.push('\''),
                _ => {
                    if let Ok(Some(c)) = reference.resolve_char_ref() {
                        text.push(c);
                    }
                }
            },
            Event::End(element) => {
                match element.local_name().as_ref() {
                    "href" => {
                        if let Some(entry) = current.as_mut() {
                            if entry.href.is_empty() {
                                entry.href = text.trim().to_string();
                            }
                        }
                    }
                    "status" => propstat_ok = text.split_whitespace().nth(1) == Some("200"),
                    "getcontentlength" => found.size = text.trim().parse().unwrap_or(0),
                    "getlastmodified" => {
                        found.modified_ms = chrono::DateTime::parse_from_rfc2822(text.trim())
                            .map(|d| d.timestamp_millis())
                            .unwrap_or(0);
                    }
                    "resourcetype" => in_resourcetype = false,
                    "propstat" => {
                        if propstat_ok {
                            any_found = true;
                            if let Some(entry) = current.as_mut() {
                                entry.is_dir |= found.is_dir;
                                entry.size = entry.size.max(found.size);
                                entry.modified_ms = entry.modified_ms.max(found.modified_ms);
                            }
                        }
                    }
                    "response" => {
                        if let Some(entry) = current.take() {
                            // An entry the server could not describe may be a
                            // directory: without it the listing is not whole,
                            // and a partial listing must not reach the sweep.
                            if !any_found {
                                return Err("an entry of the listing has no data".into());
                            }
                            if !entry.href.is_empty() {
                                entries.push(entry);
                            }
                        }
                    }
                    "multistatus" => closed = true,
                    _ => {}
                }
                text.clear();
            }
            _ => {}
        }
    }
    // quick-xml ends quietly on a body cut short. A server that died half way
    // through a listing has not said the rest of the folder is gone.
    if !closed {
        return Err("the listing was cut short".into());
    }
    Ok(entries)
}

#[cfg(test)]
mod tests;

//! ContentDirectory `Browse` SOAP handler + DIDL-Lite generator.
//!
//! # Object IDs
//!
//! Routed by string prefix:
//!   - `0`                  — Root (children: `0/artists`, `0/albums`)
//!   - `0/artists`          — All artists (containers)
//!   - `0/artists/<id>`     — Albums for that artist (containers)
//!   - `0/albums`           — All albums (containers)
//!   - `0/albums/<id>`      — Tracks for that album (items)
//!   - `0/track/<id>`       — Single track item (used by BrowseMetadata)
//!
//! Every id above answers `BrowseMetadata` with the one object it
//! names, and every container carries a `childCount`. Controllers
//! read a folder's metadata before opening it, or before queueing it
//! whole, and treat an empty answer as an empty folder.
//!
//! An album is listed under both `0/albums` and `0/artists/<id>`, but
//! its own metadata names `0/albums` as the parent: that is the
//! container its id lives under, and it is reachable from the root
//! whatever the album's artist row says. A track names its album, the
//! only container that lists it.
//!
//! # Pagination
//!
//! `StartingIndex` + `RequestedCount` map directly to SQL
//! `LIMIT … OFFSET …`. `RequestedCount = 0` means "all" per the
//! ContentDirectory spec.

use std::sync::Arc;

use axum::{
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::dlna::{
    description::xml_escape,
    http::{LocalAddr, ServerCtx},
};

/// Maximum rows we'll ever return in a single Browse call. Even when
/// the controller asks for "all" (count = 0), we cap at this value to
/// keep responses under typical DLNA controller buffer limits and
/// avoid serialising 50k tracks into one DIDL document.
const MAX_PAGE_SIZE: i64 = 500;

/// Titles of the two top-level folders. Shared by the root listing and
/// by each folder's own `BrowseMetadata` so the two cannot drift.
const ARTISTS_TITLE: &str = "Artists";
const ALBUMS_TITLE: &str = "Albums";

/// A ContentDirectory object id, parsed once so `BrowseDirectChildren`
/// and `BrowseMetadata` agree on which ids exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjectId {
    Root,
    Artists,
    Artist(i64),
    Albums,
    Album(i64),
    Track(i64),
}

impl ObjectId {
    /// `None` for anything the server never emits. An empty id is the
    /// root: some controllers send `<ObjectID></ObjectID>` on first
    /// contact.
    fn parse(oid: &str) -> Option<Self> {
        match oid {
            "" | "0" => Some(Self::Root),
            "0/artists" => Some(Self::Artists),
            "0/albums" => Some(Self::Albums),
            _ => {
                let numbered = |prefix: &str| oid.strip_prefix(prefix)?.parse::<i64>().ok();
                numbered("0/artists/")
                    .map(Self::Artist)
                    .or_else(|| numbered("0/albums/").map(Self::Album))
                    .or_else(|| numbered("0/track/").map(Self::Track))
            }
        }
    }

    /// The parent a container's own metadata names. A track's parent
    /// depends on its album row instead, see [`track_parent_id`].
    fn container_parent(self) -> &'static str {
        match self {
            Self::Root => "-1",
            Self::Artists | Self::Albums => "0",
            Self::Artist(_) => "0/artists",
            Self::Album(_) => "0/albums",
            // Never asked: a track's metadata reads its album row.
            Self::Track(_) => "0",
        }
    }
}

/// The container a track is listed under: its album, or the root for a
/// track without one (no container lists it, and the root at least
/// exists).
fn track_parent_id(album_id: Option<i64>) -> String {
    album_id.map_or_else(|| "0".to_string(), |id| format!("0/albums/{id}"))
}

/// SOAP control endpoint handler. Decodes the envelope, dispatches by
/// action name, returns the appropriate SOAP response (or fault).
pub async fn handle_control(
    State(ctx): State<Arc<ServerCtx>>,
    ConnectInfo(conn): ConnectInfo<LocalAddr>,
    body: String,
) -> Response {
    let action = match parse_soap_action(&body) {
        Some(a) => a,
        None => return soap_fault(401, "Invalid Action"),
    };

    match action {
        SoapAction::Browse(req) => match browse(&ctx, &ctx.base_url_for(conn), &req).await {
            Ok(resp) => soap_response("Browse", &resp),
            Err(err) => {
                tracing::warn!(?err, "Browse failed");
                soap_fault(501, "Action Failed")
            }
        },
        SoapAction::GetSearchCapabilities => {
            soap_response_raw("GetSearchCapabilitiesResponse", "<SearchCaps></SearchCaps>")
        }
        SoapAction::GetSortCapabilities => soap_response_raw(
            "GetSortCapabilitiesResponse",
            "<SortCaps>dc:title</SortCaps>",
        ),
        SoapAction::GetSystemUpdateID => {
            soap_response_raw("GetSystemUpdateIDResponse", "<Id>1</Id>")
        }
    }
}

#[derive(Debug)]
enum SoapAction {
    Browse(BrowseRequest),
    GetSearchCapabilities,
    GetSortCapabilities,
    GetSystemUpdateID,
}

#[derive(Debug, Default, Deserialize)]
pub struct BrowseRequest {
    #[serde(rename = "ObjectID", default)]
    pub object_id: String,
    #[serde(rename = "BrowseFlag", default)]
    pub browse_flag: String,
    #[serde(rename = "StartingIndex", default)]
    pub starting_index: i64,
    #[serde(rename = "RequestedCount", default)]
    pub requested_count: i64,
}

/// Parse the SOAP envelope just enough to identify the action and
/// pull out the Browse arguments. Falls back to a default
/// `BrowseRequest` when individual fields are missing — controllers
/// like VLC sometimes omit `SortCriteria`, and we don't sort anyway.
fn parse_soap_action(body: &str) -> Option<SoapAction> {
    if body.contains(":Browse>") || body.contains("<Browse ") || body.contains("<u:Browse") {
        let req = BrowseRequest {
            object_id: extract_tag(body, "ObjectID").unwrap_or_else(|| "0".into()),
            browse_flag: extract_tag(body, "BrowseFlag")
                .unwrap_or_else(|| "BrowseDirectChildren".into()),
            starting_index: extract_tag(body, "StartingIndex")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            requested_count: extract_tag(body, "RequestedCount")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        };
        return Some(SoapAction::Browse(req));
    }
    if body.contains("GetSearchCapabilities") {
        return Some(SoapAction::GetSearchCapabilities);
    }
    if body.contains("GetSortCapabilities") {
        return Some(SoapAction::GetSortCapabilities);
    }
    if body.contains("GetSystemUpdateID") {
        return Some(SoapAction::GetSystemUpdateID);
    }
    None
}

/// Extract the inner text of `<Tag>...</Tag>` (case-sensitive,
/// namespace-prefix-tolerant). Avoids dragging a full XML parser
/// into the hot path; SOAP envelopes are predictable enough that
/// substring scanning is fine.
fn extract_tag(body: &str, name: &str) -> Option<String> {
    let needle = format!("{name}>");
    let start = body.find(&needle)? + needle.len();
    let rest = &body[start..];
    let end = rest.find("</")?;
    let raw = &rest[..end];
    Some(xml_unescape(raw.trim()))
}

fn xml_unescape(input: &str) -> String {
    input
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Outcome of a Browse: a serialised DIDL-Lite document plus the
/// counts the SOAP envelope wraps around it.
struct BrowseResult {
    didl: String,
    number_returned: i64,
    total_matches: i64,
}

/// `base` prefixes the stream and artwork URLs: the address this
/// request reached us on (see [`ServerCtx::base_url_for`]).
async fn browse(
    ctx: &ServerCtx,
    base: &str,
    req: &BrowseRequest,
) -> Result<BrowseResult, sqlx::Error> {
    // Resolve effective limit + offset. RequestedCount = 0 means
    // "everything" per the spec — we cap at MAX_PAGE_SIZE.
    let count = if req.requested_count <= 0 {
        MAX_PAGE_SIZE
    } else {
        req.requested_count.min(MAX_PAGE_SIZE)
    };
    let offset = req.starting_index.max(0);

    if req.browse_flag == "BrowseMetadata" {
        return browse_metadata(ctx, base, &req.object_id).await;
    }

    match ObjectId::parse(&req.object_id) {
        Some(ObjectId::Root) => browse_root(&ctx.pool).await,
        Some(ObjectId::Artists) => browse_artists(&ctx.pool, count, offset).await,
        Some(ObjectId::Artist(id)) => browse_artist_albums(&ctx.pool, id, count, offset).await,
        Some(ObjectId::Albums) => browse_albums(&ctx.pool, count, offset).await,
        Some(ObjectId::Album(id)) => browse_album_tracks(ctx, base, id, count, offset).await,
        // A track has no children, and an unknown id gets an empty
        // DIDL rather than a fault so controllers gracefully render
        // an empty folder.
        Some(ObjectId::Track(_)) | None => Ok(empty_result()),
    }
}

fn empty_result() -> BrowseResult {
    BrowseResult {
        didl: didl_envelope(""),
        number_returned: 0,
        total_matches: 0,
    }
}

/// Wrap a single DIDL object, or nothing when the id named no row.
fn single_result(body: Option<String>) -> BrowseResult {
    match body {
        Some(body) => BrowseResult {
            didl: didl_envelope(&body),
            number_returned: 1,
            total_matches: 1,
        },
        None => empty_result(),
    }
}

/// Children of the two top-level folders, in the order
/// `(artists, albums)`.
async fn root_counts(pool: &SqlitePool) -> Result<(i64, i64), sqlx::Error> {
    let artists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artist")
        .fetch_one(pool)
        .await?;
    let albums: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM album")
        .fetch_one(pool)
        .await?;
    Ok((artists, albums))
}

fn artists_container(child_count: i64) -> String {
    container_xml("0/artists", "0", ARTISTS_TITLE, Some(child_count))
}

fn albums_container(child_count: i64) -> String {
    container_xml("0/albums", "0", ALBUMS_TITLE, Some(child_count))
}

async fn browse_root(pool: &SqlitePool) -> Result<BrowseResult, sqlx::Error> {
    let (artists, albums) = root_counts(pool).await?;
    Ok(root_children(artists, albums))
}

fn root_children(artists: i64, albums: i64) -> BrowseResult {
    let mut body = artists_container(artists);
    body.push_str(&albums_container(albums));
    BrowseResult {
        didl: didl_envelope(&body),
        number_returned: 2,
        total_matches: 2,
    }
}

async fn browse_artists(
    pool: &SqlitePool,
    count: i64,
    offset: i64,
) -> Result<BrowseResult, sqlx::Error> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artist")
        .fetch_one(pool)
        .await?;
    // The per-row counts ride `idx_album_artist`, so a page of 500
    // stays a page of index probes.
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT ar.id, ar.name,
                (SELECT COUNT(*) FROM album al WHERE al.artist_id = ar.id)
           FROM artist ar
          ORDER BY ar.canonical_name
          LIMIT ? OFFSET ?",
    )
    .bind(count)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let mut body = String::new();
    for (id, name, albums) in &rows {
        body.push_str(&container_xml(
            &format!("0/artists/{id}"),
            "0/artists",
            name,
            Some(*albums),
        ));
    }
    Ok(BrowseResult {
        didl: didl_envelope(&body),
        number_returned: rows.len() as i64,
        total_matches: total,
    })
}

async fn browse_artist_albums(
    pool: &SqlitePool,
    artist_id: i64,
    count: i64,
    offset: i64,
) -> Result<BrowseResult, sqlx::Error> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM album WHERE artist_id = ?")
        .bind(artist_id)
        .fetch_one(pool)
        .await?;
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT al.id, al.title,
                (SELECT COUNT(*) FROM track t WHERE t.album_id = al.id AND t.is_available = 1)
           FROM album al
          WHERE al.artist_id = ?
          ORDER BY al.year, al.canonical_title
          LIMIT ? OFFSET ?",
    )
    .bind(artist_id)
    .bind(count)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let parent = format!("0/artists/{artist_id}");
    let mut body = String::new();
    for (id, title, tracks) in &rows {
        body.push_str(&container_xml(
            &format!("0/albums/{id}"),
            &parent,
            title,
            Some(*tracks),
        ));
    }
    Ok(BrowseResult {
        didl: didl_envelope(&body),
        number_returned: rows.len() as i64,
        total_matches: total,
    })
}

async fn browse_albums(
    pool: &SqlitePool,
    count: i64,
    offset: i64,
) -> Result<BrowseResult, sqlx::Error> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM album")
        .fetch_one(pool)
        .await?;
    // Only available tracks count, because `browse_album_tracks` lists
    // only those; the per-row count rides `idx_track_album_available`.
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT al.id, al.title,
                (SELECT COUNT(*) FROM track t WHERE t.album_id = al.id AND t.is_available = 1)
           FROM album al
          ORDER BY al.canonical_title
          LIMIT ? OFFSET ?",
    )
    .bind(count)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let mut body = String::new();
    for (id, title, tracks) in &rows {
        body.push_str(&container_xml(
            &format!("0/albums/{id}"),
            "0/albums",
            title,
            Some(*tracks),
        ));
    }
    Ok(BrowseResult {
        didl: didl_envelope(&body),
        number_returned: rows.len() as i64,
        total_matches: total,
    })
}

#[derive(sqlx::FromRow)]
struct TrackRow {
    id: i64,
    title: String,
    /// Resolved via JOIN — `track.primary_artist` is the artist row
    /// id, but we want the display name in the DIDL `dc:creator`
    /// element.
    artist_name: Option<String>,
    duration_ms: i64,
    file_size: i64,
    bitrate: Option<i64>,
    sample_rate: Option<i64>,
    channels: Option<i64>,
    file_path: String,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    album_title: Option<String>,
    /// Names the track's parent container in its own metadata.
    album_id: Option<i64>,
}

async fn browse_album_tracks(
    ctx: &ServerCtx,
    base: &str,
    album_id: i64,
    count: i64,
    offset: i64,
) -> Result<BrowseResult, sqlx::Error> {
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM track WHERE album_id = ? AND is_available = 1")
            .bind(album_id)
            .fetch_one(&ctx.pool)
            .await?;
    let rows: Vec<TrackRow> = sqlx::query_as(
        "SELECT t.id, t.title,
                ar.name AS artist_name,
                t.duration_ms, t.file_size, t.bitrate, t.sample_rate, t.channels,
                t.file_path,
                aw.hash  AS artwork_hash,
                aw.format AS artwork_format,
                al.title AS album_title,
                t.album_id
           FROM track t
           LEFT JOIN album al   ON al.id = t.album_id
           LEFT JOIN artwork aw ON aw.id = al.artwork_id
           LEFT JOIN artist ar  ON ar.id = t.primary_artist
          WHERE t.album_id = ? AND t.is_available = 1
          ORDER BY t.disc_number, t.track_number, t.title
          LIMIT ? OFFSET ?",
    )
    .bind(album_id)
    .bind(count)
    .bind(offset)
    .fetch_all(&ctx.pool)
    .await?;
    let parent = format!("0/albums/{album_id}");
    let mut body = String::new();
    for row in &rows {
        body.push_str(&item_xml(base, row, &parent));
    }
    Ok(BrowseResult {
        didl: didl_envelope(&body),
        number_returned: rows.len() as i64,
        total_matches: total,
    })
}

async fn browse_metadata(
    ctx: &ServerCtx,
    base: &str,
    object_id: &str,
) -> Result<BrowseResult, sqlx::Error> {
    let body = match ObjectId::parse(object_id) {
        Some(ObjectId::Track(track_id)) => track_metadata(ctx, base, track_id).await?,
        Some(container) => container_metadata(&ctx.pool, container).await?,
        None => None,
    };
    Ok(single_result(body))
}

/// The `<container>` an id names, or `None` when its row is gone.
async fn container_metadata(
    pool: &SqlitePool,
    id: ObjectId,
) -> Result<Option<String>, sqlx::Error> {
    let parent = id.container_parent();
    Ok(match id {
        ObjectId::Root => Some(format!(
            r#"<container id="0" parentID="{parent}" restricted="1" childCount="2"><dc:title>WaveFlow</dc:title><upnp:class>object.container</upnp:class></container>"#
        )),
        ObjectId::Artists => Some(artists_container(root_counts(pool).await?.0)),
        ObjectId::Albums => Some(albums_container(root_counts(pool).await?.1)),
        ObjectId::Artist(artist_id) => {
            let row: Option<(String, i64)> = sqlx::query_as(
                "SELECT ar.name, (SELECT COUNT(*) FROM album al WHERE al.artist_id = ar.id)
                   FROM artist ar
                  WHERE ar.id = ?",
            )
            .bind(artist_id)
            .fetch_optional(pool)
            .await?;
            row.map(|(name, albums)| {
                container_xml(
                    &format!("0/artists/{artist_id}"),
                    parent,
                    &name,
                    Some(albums),
                )
            })
        }
        ObjectId::Album(album_id) => {
            let row: Option<(String, i64)> = sqlx::query_as(
                "SELECT al.title,
                        (SELECT COUNT(*) FROM track t WHERE t.album_id = al.id AND t.is_available = 1)
                   FROM album al
                  WHERE al.id = ?",
            )
            .bind(album_id)
            .fetch_optional(pool)
            .await?;
            row.map(|(title, tracks)| {
                container_xml(
                    &format!("0/albums/{album_id}"),
                    parent,
                    &title,
                    Some(tracks),
                )
            })
        }
        ObjectId::Track(_) => None,
    })
}

async fn track_metadata(
    ctx: &ServerCtx,
    base: &str,
    track_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    // Column aliases must match `TrackRow`: `ar.name AS artist_name`
    // (not `t.primary_artist`) and `t.bitrate AS bitrate` (the column
    // is `bitrate`, not `bit_rate` — the earlier shape would fail at
    // bind time on every BrowseMetadata request).
    let row: Option<TrackRow> = sqlx::query_as(
        "SELECT t.id, t.title,
                ar.name AS artist_name,
                t.duration_ms, t.file_size, t.bitrate, t.sample_rate, t.channels,
                t.file_path,
                aw.hash  AS artwork_hash,
                aw.format AS artwork_format,
                al.title AS album_title,
                t.album_id
           FROM track t
           LEFT JOIN album al   ON al.id = t.album_id
           LEFT JOIN artwork aw ON aw.id = al.artwork_id
           LEFT JOIN artist ar  ON ar.id = t.primary_artist
          WHERE t.id = ? AND t.is_available = 1",
    )
    .bind(track_id)
    .fetch_optional(&ctx.pool)
    .await?;
    Ok(row.map(|row| item_xml(base, &row, &track_parent_id(row.album_id))))
}

/// Wrap inner content in the standard DIDL-Lite envelope. The four
/// xmlns declarations are mandatory for AVTransport-side parsers.
fn didl_envelope(inner: &str) -> String {
    format!(
        r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/">{inner}</DIDL-Lite>"#
    )
}

fn container_xml(id: &str, parent: &str, title: &str, child_count: Option<i64>) -> String {
    let cc = child_count
        .map(|n| format!(r#" childCount="{n}""#))
        .unwrap_or_default();
    format!(
        r#"<container id="{id}" parentID="{parent}" restricted="1"{cc}><dc:title>{title}</dc:title><upnp:class>object.container.storageFolder</upnp:class></container>"#,
        title = xml_escape(title)
    )
}

fn item_xml(base: &str, t: &TrackRow, parent: &str) -> String {
    let mime = mime_for_path(&t.file_path);
    let stream_url = format!("{base}/stream/{}", t.id);
    let duration = format_duration_hms(t.duration_ms);
    let size_attr = format!(r#" size="{}""#, t.file_size);
    let bitrate_attr = t
        .bitrate
        .map(|b| format!(r#" bitrate="{}""#, b * 1000 / 8)) // kbps → bytes/s for DLNA
        .unwrap_or_default();
    let sr_attr = t
        .sample_rate
        .map(|s| format!(r#" sampleFrequency="{s}""#))
        .unwrap_or_default();
    let nc_attr = t
        .channels
        .map(|c| format!(r#" nrAudioChannels="{c}""#))
        .unwrap_or_default();
    let artist_xml = t
        .artist_name
        .as_deref()
        .map(|a| {
            format!(
                "<dc:creator>{}</dc:creator><upnp:artist>{}</upnp:artist>",
                xml_escape(a),
                xml_escape(a)
            )
        })
        .unwrap_or_default();
    let album_xml = t
        .album_title
        .as_deref()
        .map(|a| format!("<upnp:album>{}</upnp:album>", xml_escape(a)))
        .unwrap_or_default();
    let art_xml = t
        .artwork_hash
        .as_deref()
        .map(|h| {
            let ext = t.artwork_format.as_deref().unwrap_or("jpg");
            format!(
                r#"<upnp:albumArtURI dlna:profileID="JPEG_TN" xmlns:dlna="urn:schemas-dlna-org:metadata-1-0/">{base}/art/{h}.{ext}</upnp:albumArtURI>"#,
            )
        })
        .unwrap_or_default();
    let protocol_info = format!(
        "http-get:*:{mime}:DLNA.ORG_OP=01;DLNA.ORG_FLAGS=01700000000000000000000000000000",
    );
    format!(
        r#"<item id="0/track/{id}" parentID="{parent}" restricted="1"><dc:title>{title}</dc:title>{artist_xml}{album_xml}{art_xml}<upnp:class>object.item.audioItem.musicTrack</upnp:class><res protocolInfo="{protocol_info}" duration="{duration}"{size_attr}{bitrate_attr}{sr_attr}{nc_attr}>{stream_url}</res></item>"#,
        id = t.id,
        title = xml_escape(&t.title),
    )
}

fn mime_for_path(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("mp3") => "audio/mpeg",
        Some("flac") => "audio/flac",
        Some("wav") => "audio/wav",
        // `x-aiff` rather than `aiff`: it is what Apple emits and what
        // renderers actually match on.
        Some("aiff" | "aif") => "audio/x-aiff",
        Some("ogg" | "oga") => "audio/ogg",
        Some("m4a" | "mp4" | "aac") => "audio/mp4",
        _ => "application/octet-stream",
    }
}

/// Format duration as `H:MM:SS.FFF` per the DLNA `res@duration`
/// requirement. Most controllers accept the simpler `HH:MM:SS` too,
/// but the trailing `.000` keeps Sonos S2 happy.
fn format_duration_hms(ms: i64) -> String {
    let total_s = ms / 1000;
    let h = total_s / 3600;
    let m = (total_s % 3600) / 60;
    let s = total_s % 60;
    format!("{h}:{m:02}:{s:02}.000")
}

fn soap_response(action: &str, result: &BrowseResult) -> Response {
    let escaped_didl = xml_escape(&result.didl);
    let body = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:{action}Response xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1">
      <Result>{escaped_didl}</Result>
      <NumberReturned>{nr}</NumberReturned>
      <TotalMatches>{tm}</TotalMatches>
      <UpdateID>1</UpdateID>
    </u:{action}Response>
  </s:Body>
</s:Envelope>"#,
        nr = result.number_returned,
        tm = result.total_matches,
    );
    soap_ok(body)
}

fn soap_response_raw(action: &str, payload: &str) -> Response {
    let body = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:{action} xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1">{payload}</u:{action}>
  </s:Body>
</s:Envelope>"#
    );
    soap_ok(body)
}

fn soap_ok(body: String) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "text/xml; charset=\"utf-8\"".parse().unwrap(),
    );
    (StatusCode::OK, headers, body).into_response()
}

fn soap_fault(code: u16, msg: &str) -> Response {
    let body = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <s:Fault>
      <faultcode>s:Client</faultcode>
      <faultstring>UPnPError</faultstring>
      <detail>
        <UPnPError xmlns="urn:schemas-upnp-org:control-1-0">
          <errorCode>{code}</errorCode>
          <errorDescription>{msg}</errorDescription>
        </UPnPError>
      </detail>
    </s:Fault>
  </s:Body>
</s:Envelope>"#
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "text/xml; charset=\"utf-8\"".parse().unwrap(),
    );
    (StatusCode::INTERNAL_SERVER_ERROR, headers, body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tag_handles_namespace_prefixes() {
        let body = "<u:Browse><ObjectID>0/albums</ObjectID></u:Browse>";
        assert_eq!(extract_tag(body, "ObjectID"), Some("0/albums".into()));
    }

    #[test]
    fn extract_tag_unescapes_xml_entities() {
        let body = "<ObjectID>0/path&amp;weird</ObjectID>";
        assert_eq!(extract_tag(body, "ObjectID"), Some("0/path&weird".into()));
    }

    #[test]
    fn parse_soap_action_recognises_browse() {
        let body = r#"<s:Envelope><s:Body><u:Browse xmlns:u="..."><ObjectID>0</ObjectID><BrowseFlag>BrowseDirectChildren</BrowseFlag><StartingIndex>5</StartingIndex><RequestedCount>20</RequestedCount></u:Browse></s:Body></s:Envelope>"#;
        match parse_soap_action(body) {
            Some(SoapAction::Browse(req)) => {
                assert_eq!(req.object_id, "0");
                assert_eq!(req.starting_index, 5);
                assert_eq!(req.requested_count, 20);
            }
            _ => panic!("expected Browse"),
        }
    }

    #[test]
    fn duration_formats_hms_with_ms_padding() {
        assert_eq!(format_duration_hms(0), "0:00:00.000");
        assert_eq!(format_duration_hms(3_661_000), "1:01:01.000");
    }

    #[test]
    fn root_browse_lists_two_top_level_containers() {
        let r = root_children(3, 7);
        assert_eq!(r.number_returned, 2);
        assert!(r
            .didl
            .contains(r#"id="0/artists" parentID="0" restricted="1" childCount="3""#));
        assert!(r
            .didl
            .contains(r#"id="0/albums" parentID="0" restricted="1" childCount="7""#));
    }

    #[test]
    fn object_id_parses_every_id_the_server_emits() {
        assert_eq!(ObjectId::parse(""), Some(ObjectId::Root));
        assert_eq!(ObjectId::parse("0"), Some(ObjectId::Root));
        assert_eq!(ObjectId::parse("0/artists"), Some(ObjectId::Artists));
        assert_eq!(ObjectId::parse("0/artists/12"), Some(ObjectId::Artist(12)));
        assert_eq!(ObjectId::parse("0/albums"), Some(ObjectId::Albums));
        assert_eq!(ObjectId::parse("0/albums/34"), Some(ObjectId::Album(34)));
        assert_eq!(ObjectId::parse("0/track/56"), Some(ObjectId::Track(56)));
    }

    #[test]
    fn object_id_rejects_what_the_server_never_emits() {
        assert_eq!(ObjectId::parse("0/artists/"), None);
        assert_eq!(ObjectId::parse("0/albums/abc"), None);
        assert_eq!(ObjectId::parse("0/track"), None);
        assert_eq!(ObjectId::parse("0/genres"), None);
        assert_eq!(ObjectId::parse("1"), None);
    }

    #[test]
    fn every_container_names_the_container_that_lists_it() {
        assert_eq!(ObjectId::Root.container_parent(), "-1");
        assert_eq!(ObjectId::Artists.container_parent(), "0");
        assert_eq!(ObjectId::Albums.container_parent(), "0");
        assert_eq!(ObjectId::Artist(1).container_parent(), "0/artists");
        assert_eq!(ObjectId::Album(1).container_parent(), "0/albums");
    }

    #[test]
    fn a_track_names_its_album_or_the_root() {
        assert_eq!(track_parent_id(Some(9)), "0/albums/9");
        assert_eq!(track_parent_id(None), "0");
    }

    #[test]
    fn single_result_counts_one_object_or_none() {
        let one = single_result(Some(container_xml(
            "0/albums/1",
            "0/albums",
            "A & B",
            Some(4),
        )));
        assert_eq!((one.number_returned, one.total_matches), (1, 1));
        assert!(one
            .didl
            .contains(r#"childCount="4"><dc:title>A &amp; B</dc:title>"#));
        let none = single_result(None);
        assert_eq!((none.number_returned, none.total_matches), (0, 0));
    }
}

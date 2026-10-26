//! ContentDirectory `Browse` SOAP handler + DIDL-Lite generator.
//!
//! # Object IDs
//!
//! Routed by string prefix:
//!   - `0`                    — Root (children: the five folders below)
//!   - `0/artists`            — All artists (containers)
//!   - `0/artists/<id>`       — Albums for that artist (containers)
//!   - `0/albums`             — All albums (containers)
//!   - `0/albums/<id>`        — Tracks for that album (items)
//!   - `0/genres`             — Genres holding an available track (containers)
//!   - `0/genres/<id>`        — Tracks of that genre (items)
//!   - `0/playlists`          — All playlists, smart ones included (containers)
//!   - `0/playlists/<id>`     — Tracks of that playlist, in its order (items)
//!   - `0/folders`            — The library folders (containers)
//!   - `0/folders/<id>[/<segment>…]` — A directory under one of them:
//!     its subdirectories (containers), then its files (items)
//!   - `0/track/<id>`         — Single track item (used by BrowseMetadata)
//!
//! A directory segment is percent-encoded, every byte outside
//! `A-Z a-z 0-9 - . _ ~`, so an id stays plain ASCII: no XML escaping,
//! no `/` inside a name, and no space at an end for a controller or
//! [`extract_tag`] to trim away.
//!
//! Every id above answers `BrowseMetadata` with the one object it
//! names, and every container carries a `childCount`. Controllers
//! read a folder's metadata before opening it, or before queueing it
//! whole, and treat an empty answer as an empty folder.
//!
//! An album is listed under both `0/albums` and `0/artists/<id>`, but
//! its own metadata names `0/albums` as the parent: that is the
//! container its id lives under, and it is reachable from the root
//! whatever the album's artist row says. A track is listed under its
//! album, its genres, its playlists and its directory, and its own
//! metadata names its album: the one container that always lists it
//! whatever the user has done with genres and playlists.
//!
//! # Titles
//!
//! The five top-level titles follow the interface language. The
//! backend has no copy of it — i18next keeps it in the webview — so the
//! frontend pushes them through `dlna_set_labels`, as it pushes the
//! tray's. Until that push lands they are English.
//!
//! # Pagination
//!
//! `StartingIndex` + `RequestedCount` map directly to SQL
//! `LIMIT … OFFSET …`. `RequestedCount = 0` means "all" per the
//! ContentDirectory spec.

use std::sync::{Arc, RwLock};

use axum::{
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::{
    commands::browse::folder_prefix_range,
    dlna::{
        description::xml_escape,
        http::{LocalAddr, ServerCtx},
    },
};

use browse::{
    browse_album_tracks, browse_albums, browse_artist_albums, browse_artists, browse_folder,
    browse_genre_tracks, browse_genres, browse_library_folders, browse_playlist_tracks,
    browse_playlists,
};
use didl::{browse_metadata, container_xml, didl_envelope, item_xml};

/// Maximum rows we'll ever return in a single Browse call. Even when
/// the controller asks for "all" (count = 0), we cap at this value to
/// keep responses under typical DLNA controller buffer limits and
/// avoid serialising 50k tracks into one DIDL document.
const MAX_PAGE_SIZE: i64 = 500;

/// Titles of the top-level folders, in the interface language. Shared
/// by the root listing and by each folder's own `BrowseMetadata` so the
/// two cannot drift.
///
/// The frontend sends the strings of its own library tabs, so a folder
/// on the TV is named as the matching tab in the app.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootLabels {
    pub artists: String,
    pub albums: String,
    pub genres: String,
    pub playlists: String,
    pub folders: String,
}

/// English, for the stretch between the server starting at boot and
/// the frontend's first push.
impl Default for RootLabels {
    fn default() -> Self {
        Self {
            artists: "Artists".into(),
            albums: "Albums".into(),
            genres: "Genres".into(),
            playlists: "Playlists".into(),
            folders: "Folders".into(),
        }
    }
}

/// Written by `dlna_set_labels`, read on every Browse: a language
/// change shows on the next request, without restarting the server.
pub type SharedLabels = Arc<RwLock<RootLabels>>;

/// A snapshot of the titles. A poisoned lock still holds whole strings
/// (the writer only assigns), so it is read rather than refused.
fn current_labels(labels: &SharedLabels) -> RootLabels {
    match labels.read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// A ContentDirectory object id, parsed once so `BrowseDirectChildren`
/// and `BrowseMetadata` agree on which ids exist.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ObjectId {
    Root,
    Artists,
    Artist(i64),
    Albums,
    Album(i64),
    Genres,
    Genre(i64),
    Playlists,
    Playlist(i64),
    Folders,
    /// A directory under the `library_folder` row `root`: the folder
    /// itself when `rel` is empty, else the directory that many levels
    /// below it, one decoded name per level.
    Folder {
        root: i64,
        rel: Vec<String>,
    },
    Track(i64),
}

/// The top-level folders, in the order the root lists them.
const TOP_LEVEL: [ObjectId; 5] = [
    ObjectId::Artists,
    ObjectId::Albums,
    ObjectId::Genres,
    ObjectId::Playlists,
    ObjectId::Folders,
];

impl ObjectId {
    /// `None` for anything the server never emits. An empty id is the
    /// root: some controllers send `<ObjectID></ObjectID>` on first
    /// contact.
    fn parse(oid: &str) -> Option<Self> {
        match oid {
            "" | "0" => Some(Self::Root),
            "0/artists" => Some(Self::Artists),
            "0/albums" => Some(Self::Albums),
            "0/genres" => Some(Self::Genres),
            "0/playlists" => Some(Self::Playlists),
            "0/folders" => Some(Self::Folders),
            _ => {
                if let Some(rest) = oid.strip_prefix("0/folders/") {
                    let mut parts = rest.split('/');
                    let root = parts.next()?.parse::<i64>().ok()?;
                    let rel = parts.map(decode_segment).collect::<Option<Vec<_>>>()?;
                    return Some(Self::Folder { root, rel });
                }
                let numbered = |prefix: &str| oid.strip_prefix(prefix)?.parse::<i64>().ok();
                numbered("0/artists/")
                    .map(Self::Artist)
                    .or_else(|| numbered("0/albums/").map(Self::Album))
                    .or_else(|| numbered("0/genres/").map(Self::Genre))
                    .or_else(|| numbered("0/playlists/").map(Self::Playlist))
                    .or_else(|| numbered("0/track/").map(Self::Track))
            }
        }
    }

    /// The id string, the inverse of [`Self::parse`].
    fn to_id(&self) -> String {
        match self {
            Self::Root => "0".into(),
            Self::Artists => "0/artists".into(),
            Self::Artist(id) => format!("0/artists/{id}"),
            Self::Albums => "0/albums".into(),
            Self::Album(id) => format!("0/albums/{id}"),
            Self::Genres => "0/genres".into(),
            Self::Genre(id) => format!("0/genres/{id}"),
            Self::Playlists => "0/playlists".into(),
            Self::Playlist(id) => format!("0/playlists/{id}"),
            Self::Folders => "0/folders".into(),
            Self::Folder { root, rel } => {
                let mut id = format!("0/folders/{root}");
                for segment in rel {
                    id.push('/');
                    id.push_str(&encode_segment(segment));
                }
                id
            }
            Self::Track(id) => format!("0/track/{id}"),
        }
    }

    /// The parent a container's own metadata names. A track's parent
    /// depends on its album row instead, see [`track_parent_id`].
    fn container_parent(&self) -> String {
        match self {
            Self::Root => "-1".into(),
            Self::Artists | Self::Albums | Self::Genres | Self::Playlists | Self::Folders => {
                "0".into()
            }
            Self::Artist(_) => "0/artists".into(),
            Self::Album(_) => "0/albums".into(),
            Self::Genre(_) => "0/genres".into(),
            Self::Playlist(_) => "0/playlists".into(),
            Self::Folder { rel, .. } if rel.is_empty() => "0/folders".into(),
            Self::Folder { root, rel } => Self::Folder {
                root: *root,
                rel: rel[..rel.len() - 1].to_vec(),
            }
            .to_id(),
            // Never asked: a track's metadata reads its album row.
            Self::Track(_) => "0".into(),
        }
    }
}

/// Percent-encode one directory name for an object id.
fn encode_segment(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The name [`encode_segment`] encoded, or `None` for a segment the
/// server never emits: empty, `.` / `..`, a separator inside, a broken
/// escape or bytes that are not UTF-8.
fn decode_segment(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let name = String::from_utf8(out).ok()?;
    let valid = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains(std::path::MAIN_SEPARATOR);
    valid.then_some(name)
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

/// The `SELECT … FROM … JOIN` every track listing shares, ending where
/// a listing adds its own `JOIN` / `WHERE` / `ORDER BY`. A macro rather
/// than a `const` so `concat!` keeps each query a `&'static str`.
///
/// The aliases must match [`TrackRow`]: `ar.name AS artist_name` (not
/// `t.primary_artist`) and `t.bitrate` (the column is `bitrate`, not
/// `bit_rate` — an earlier shape failed at bind time on every
/// BrowseMetadata request).
macro_rules! track_select {
    ($tail:literal) => {
        concat!(
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
             ",
            $tail
        )
    };
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

    let pool = &ctx.pool;
    match ObjectId::parse(&req.object_id) {
        Some(ObjectId::Root) => {
            let counts = root_counts(pool).await?;
            Ok(root_children(
                &current_labels(&ctx.labels),
                counts,
                count,
                offset,
            ))
        }
        Some(ObjectId::Artists) => browse_artists(pool, count, offset).await,
        Some(ObjectId::Artist(id)) => browse_artist_albums(pool, id, count, offset).await,
        Some(ObjectId::Albums) => browse_albums(pool, count, offset).await,
        Some(ObjectId::Album(id)) => browse_album_tracks(ctx, base, id, count, offset).await,
        Some(ObjectId::Genres) => browse_genres(pool, count, offset).await,
        Some(ObjectId::Genre(id)) => browse_genre_tracks(ctx, base, id, count, offset).await,
        Some(ObjectId::Playlists) => browse_playlists(pool, count, offset).await,
        Some(ObjectId::Playlist(id)) => browse_playlist_tracks(ctx, base, id, count, offset).await,
        Some(ObjectId::Folders) => browse_library_folders(pool, count, offset).await,
        Some(ObjectId::Folder { root, rel }) => {
            browse_folder(ctx, base, root, &rel, count, offset).await
        }
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

/// A page of `(id, title, childCount)` containers, all under `parent`,
/// each id turned into its object id by `child_id`.
fn containers_page(
    parent: &str,
    rows: &[(i64, String, i64)],
    total: i64,
    child_id: impl Fn(i64) -> String,
) -> BrowseResult {
    let mut body = String::new();
    for (id, title, children) in rows {
        body.push_str(&container_xml(
            &child_id(*id),
            parent,
            title,
            Some(*children),
        ));
    }
    BrowseResult {
        didl: didl_envelope(&body),
        number_returned: rows.len() as i64,
        total_matches: total,
    }
}

/// A page of track items, all listed under `parent`.
fn items_result(base: &str, parent: &str, rows: &[TrackRow], total: i64) -> BrowseResult {
    let mut body = String::new();
    for row in rows {
        body.push_str(&item_xml(base, row, parent));
    }
    BrowseResult {
        didl: didl_envelope(&body),
        number_returned: rows.len() as i64,
        total_matches: total,
    }
}

/// A genre and the available tracks it holds. The root and
/// `0/genres` list only the genres where that count is positive: a
/// genre whose every track went missing would open on nothing.
macro_rules! genre_rows {
    () => {
        "SELECT g.id, g.name,
                (SELECT COUNT(*)
                   FROM track_genre tg
                   JOIN track t ON t.id = tg.track_id
                  WHERE tg.genre_id = g.id AND t.is_available = 1) AS tracks
           FROM genre g"
    };
}

/// A playlist and the available tracks it holds, as its own listing
/// counts them. Smart playlists are materialised into
/// `playlist_track` like any other, so they need no special case.
macro_rules! playlist_rows {
    () => {
        "SELECT p.id, p.name, p.position, p.updated_at,
                (SELECT COUNT(*)
                   FROM playlist_track pt
                   JOIN track t ON t.id = pt.track_id
                  WHERE pt.playlist_id = p.id AND t.is_available = 1) AS tracks
           FROM playlist p"
    };
}

/// Children of the top-level folders, in [`TOP_LEVEL`] order.
async fn root_counts(pool: &SqlitePool) -> Result<[i64; 5], sqlx::Error> {
    let (artists, albums, genres, playlists, folders): (i64, i64, i64, i64, i64) =
        sqlx::query_as(concat!(
            "SELECT (SELECT COUNT(*) FROM artist),
                    (SELECT COUNT(*) FROM album),
                    (SELECT COUNT(*) FROM (",
            genre_rows!(),
            ") WHERE tracks > 0),
                    (SELECT COUNT(*) FROM playlist),
                    (SELECT COUNT(*) FROM library_folder)"
        ))
        .fetch_one(pool)
        .await?;
    Ok([artists, albums, genres, playlists, folders])
}

/// The `<container>` of one top-level folder, or `None` for an id
/// that is not one.
fn top_level_container(id: &ObjectId, labels: &RootLabels, child_count: i64) -> Option<String> {
    let title = match id {
        ObjectId::Artists => &labels.artists,
        ObjectId::Albums => &labels.albums,
        ObjectId::Genres => &labels.genres,
        ObjectId::Playlists => &labels.playlists,
        ObjectId::Folders => &labels.folders,
        _ => return None,
    };
    Some(container_xml(
        &id.to_id(),
        &id.container_parent(),
        title,
        Some(child_count),
    ))
}

/// The root's page of [`TOP_LEVEL`], `counts` in the same order.
fn root_children(labels: &RootLabels, counts: [i64; 5], count: i64, offset: i64) -> BrowseResult {
    let skip = usize::try_from(offset).unwrap_or(usize::MAX);
    let take = usize::try_from(count).unwrap_or(0);
    let mut body = String::new();
    let mut returned = 0;
    for (id, children) in TOP_LEVEL.iter().zip(counts).skip(skip).take(take) {
        if let Some(xml) = top_level_container(id, labels, children) {
            body.push_str(&xml);
            returned += 1;
        }
    }
    BrowseResult {
        didl: didl_envelope(&body),
        number_returned: returned,
        total_matches: TOP_LEVEL.len() as i64,
    }
}

/// A directory under a library folder, as the band of file paths below
/// it (see [`folder_prefix_range`]).
struct FolderDir {
    library_id: i64,
    low: String,
    high: String,
}

impl FolderDir {
    /// `rel` joins the root with the OS separator, the one the scanner
    /// stored the paths with. A root that already ends with it (`E:\`,
    /// `/`) does not get a second one.
    fn new(library_id: i64, root_path: &str, rel: &[String]) -> Self {
        let sep = std::path::MAIN_SEPARATOR;
        let dir = if rel.is_empty() {
            root_path.to_string()
        } else {
            format!(
                "{}{sep}{}",
                root_path.trim_end_matches(sep),
                rel.join(&sep.to_string())
            )
        };
        let (low, high) = folder_prefix_range(&dir);
        Self {
            library_id,
            low,
            high,
        }
    }

    /// Characters, not bytes: SQLite's `substr` and `instr` count
    /// characters, so a path with an accent would be cut mid-name by a
    /// byte length.
    fn prefix_chars(&self) -> i64 {
        self.low.chars().count() as i64
    }
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

// Declared after the row macros above, which `macro_rules!` scoping
// makes visible only to what follows them.
mod browse;
mod didl;

#[cfg(test)]
mod tests;

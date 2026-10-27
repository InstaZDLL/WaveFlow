//! `BrowseMetadata` answers and the DIDL-Lite XML every answer is written in.

use crate::dlna::description::xml_escape;
use crate::dlna::http::ServerCtx;

use super::browse::folder_child_count;
use super::{
    current_labels, root_counts, single_result, top_level_container, track_parent_id, BrowseResult,
    FolderDir, ObjectId, TrackRow, TOP_LEVEL,
};

pub(super) async fn browse_metadata(
    ctx: &ServerCtx,
    base: &str,
    object_id: &str,
) -> Result<BrowseResult, sqlx::Error> {
    let body = match ObjectId::parse(object_id) {
        Some(ObjectId::Track(track_id)) => track_metadata(ctx, base, track_id).await?,
        Some(container) => container_metadata(ctx, &container).await?,
        None => None,
    };
    Ok(single_result(body))
}

/// The `<container>` an id names, or `None` when its row is gone.
async fn container_metadata(ctx: &ServerCtx, id: &ObjectId) -> Result<Option<String>, sqlx::Error> {
    let pool = &ctx.pool;
    let own_id = id.to_id();
    let parent = id.container_parent();
    Ok(match id {
        ObjectId::Root => Some(format!(
            r#"<container id="0" parentID="{parent}" restricted="1" childCount="{}"><dc:title>WaveFlow</dc:title><upnp:class>object.container</upnp:class></container>"#,
            TOP_LEVEL.len()
        )),
        ObjectId::Artists
        | ObjectId::Albums
        | ObjectId::Genres
        | ObjectId::Playlists
        | ObjectId::Folders => {
            let counts = root_counts(pool).await?;
            let children = TOP_LEVEL
                .iter()
                .position(|top| top == id)
                .map_or(0, |i| counts[i]);
            top_level_container(id, &current_labels(&ctx.labels), children)
        }
        ObjectId::Artist(artist_id) => {
            let row: Option<(String, i64)> = sqlx::query_as(
                "SELECT ar.name, (SELECT COUNT(*) FROM album al WHERE al.artist_id = ar.id)
                   FROM artist ar
                  WHERE ar.id = ?",
            )
            .bind(*artist_id)
            .fetch_optional(pool)
            .await?;
            row.map(|(name, albums)| container_xml(&own_id, &parent, &name, Some(albums)))
        }
        ObjectId::Album(album_id) => {
            let row: Option<(String, i64)> = sqlx::query_as(
                "SELECT al.title,
                        (SELECT COUNT(*) FROM track t WHERE t.album_id = al.id AND t.is_available = 1)
                   FROM album al
                  WHERE al.id = ?",
            )
            .bind(*album_id)
            .fetch_optional(pool)
            .await?;
            row.map(|(title, tracks)| container_xml(&own_id, &parent, &title, Some(tracks)))
        }
        ObjectId::Genre(genre_id) => {
            let row: Option<(String, i64)> = sqlx::query_as(concat!(
                "SELECT name, tracks FROM (",
                genre_rows!(),
                ") WHERE id = ?"
            ))
            .bind(*genre_id)
            .fetch_optional(pool)
            .await?;
            row.map(|(name, tracks)| container_xml(&own_id, &parent, &name, Some(tracks)))
        }
        ObjectId::Playlist(playlist_id) => {
            let row: Option<(String, i64)> = sqlx::query_as(concat!(
                "SELECT name, tracks FROM (",
                playlist_rows!(),
                ") WHERE id = ?"
            ))
            .bind(*playlist_id)
            .fetch_optional(pool)
            .await?;
            row.map(|(name, tracks)| container_xml(&own_id, &parent, &name, Some(tracks)))
        }
        ObjectId::Folder { root, rel } => {
            let row: Option<(i64, String)> =
                sqlx::query_as("SELECT library_id, path FROM library_folder WHERE id = ?")
                    .bind(*root)
                    .fetch_optional(pool)
                    .await?;
            match row {
                None => None,
                Some((library_id, path)) => {
                    let dir = FolderDir::new(library_id, &path, rel);
                    let children = folder_child_count(pool, &dir).await?;
                    match rel.last() {
                        // A library folder exists while its row does,
                        // even before its first scan.
                        None => Some(container_xml(&own_id, &parent, &path, Some(children))),
                        // A directory below it exists only through the
                        // tracks under it.
                        Some(_) if children == 0 => None,
                        Some(name) => Some(container_xml(&own_id, &parent, name, Some(children))),
                    }
                }
            }
        }
        ObjectId::Track(_) => None,
    })
}

async fn track_metadata(
    ctx: &ServerCtx,
    base: &str,
    track_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<TrackRow> =
        sqlx::query_as(track_select!("WHERE t.id = ? AND t.is_available = 1"))
            .bind(track_id)
            .fetch_optional(&ctx.pool)
            .await?;
    Ok(row.map(|row| item_xml(base, &row, &track_parent_id(row.album_id))))
}

/// Wrap inner content in the standard DIDL-Lite envelope. The four
/// xmlns declarations are mandatory for AVTransport-side parsers.
pub(super) fn didl_envelope(inner: &str) -> String {
    format!(
        r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/">{inner}</DIDL-Lite>"#
    )
}

pub(super) fn container_xml(
    id: &str,
    parent: &str,
    title: &str,
    child_count: Option<i64>,
) -> String {
    let cc = child_count
        .map(|n| format!(r#" childCount="{n}""#))
        .unwrap_or_default();
    format!(
        r#"<container id="{id}" parentID="{parent}" restricted="1"{cc}><dc:title>{title}</dc:title><upnp:class>object.container.storageFolder</upnp:class></container>"#,
        title = xml_escape(title)
    )
}

pub(super) fn item_xml(base: &str, t: &TrackRow, parent: &str) -> String {
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
pub(super) fn format_duration_hms(ms: i64) -> String {
    let total_s = ms / 1000;
    let h = total_s / 3600;
    let m = (total_s % 3600) / 60;
    let s = total_s % 60;
    format!("{h}:{m:02}:{s:02}.000")
}

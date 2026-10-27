//! A playlist as a JSON export.
//!
//! Two shapes are accepted: a bare array of tracks, or an object holding
//! one under `tracks` (or `items`), with the playlist's name in `name` or
//! `title`. Each track is an object:
//!
//! - `title` (or `name`, `track`) — required;
//! - `artist` (or `artists`) — a string, an array of strings, or an array
//!   of objects with a `name`;
//! - `album` — a string, or an object with a `title` or `name`;
//! - `duration_ms` in milliseconds, or `duration` in seconds or `m:ss`;
//! - `isrc`.
//!
//! An entry that is not an object, or has no title, is skipped.

use serde_json::Value;

use super::{non_empty, normalize_isrc, parse_duration, ImportEntry, ParseError, ParsedPlaylist};

fn string_at<'a>(item: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| item.get(*key).and_then(Value::as_str))
}

/// A name out of a string or an object carrying one.
fn name_of(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => non_empty(s),
        Value::Object(_) => string_at(value, &["name", "title"]).and_then(non_empty),
        _ => None,
    }
}

fn artist_of(item: &Value) -> Option<String> {
    let value = item.get("artist").or_else(|| item.get("artists"))?;
    match value {
        Value::Array(names) => {
            let names: Vec<String> = names.iter().filter_map(name_of).collect();
            // `"; "`, the separator the library's own multi-artist split
            // uses, so the matcher reads them back the same way.
            (!names.is_empty()).then(|| names.join("; "))
        }
        other => name_of(other),
    }
}

fn duration_of(item: &Value) -> Option<i64> {
    let read = |value: &Value, as_ms: bool| match value {
        Value::Number(n) => n
            .as_f64()
            .and_then(|n| parse_duration(&n.to_string(), as_ms)),
        Value::String(s) => parse_duration(s, as_ms),
        _ => None,
    };
    item.get("duration_ms")
        .and_then(|v| read(v, true))
        .or_else(|| item.get("duration").and_then(|v| read(v, false)))
}

/// Read a JSON playlist.
pub fn parse(text: &str) -> Result<ParsedPlaylist, ParseError> {
    let root: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| ParseError::Json(e.to_string()))?;
    let (name, tracks) = match &root {
        Value::Array(tracks) => (None, tracks),
        Value::Object(_) => {
            let tracks = root
                .get("tracks")
                .or_else(|| root.get("items"))
                .and_then(Value::as_array)
                .ok_or_else(|| ParseError::Json("no `tracks` array".to_string()))?;
            (
                string_at(&root, &["name", "title"]).and_then(non_empty),
                tracks,
            )
        }
        _ => {
            return Err(ParseError::Json(
                "neither an array nor an object".to_string(),
            ))
        }
    };

    let mut entries = Vec::new();
    for item in tracks {
        if !item.is_object() {
            continue;
        }
        let Some(title) = string_at(item, &["title", "name", "track"]).and_then(non_empty) else {
            continue;
        };
        entries.push(ImportEntry {
            title,
            artist: artist_of(item),
            album: item.get("album").and_then(name_of),
            duration_ms: duration_of(item),
            isrc: string_at(item, &["isrc"]).and_then(normalize_isrc),
            locator: None,
        });
        if entries.len() >= super::MAX_ENTRIES {
            break;
        }
    }
    if entries.is_empty() {
        return Err(ParseError::Empty);
    }
    Ok(ParsedPlaylist { name, entries })
}

#[cfg(test)]
mod tests;

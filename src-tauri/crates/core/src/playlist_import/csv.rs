//! A playlist as a CSV export.
//!
//! The header is required and its names are what the columns mean; their
//! order does not matter. Each column is recognised under the names the
//! common exports give it, compared without case or surrounding spaces:
//!
//! | Meaning  | Header names                                                   |
//! | -------- | -------------------------------------------------------------- |
//! | title    | `title`, `track`, `track name`, `track title`, `name`, `song`  |
//! | artist   | `artist`, `artists`, `artist name`, `artist name(s)`, `artist(s)` |
//! | album    | `album`, `album name`, `album title`, `release`                |
//! | duration | `duration_ms`, `duration (ms)`, `duration ms` — milliseconds;  |
//! |          | `duration`, `length`, `time`, `duration (s)` — see [`super::parse_duration`] |
//! | ISRC     | `isrc`                                                         |
//!
//! Only the title is required. The separator is whichever of `,`, `;` and
//! tab the header line holds most of outside quotes, so a spreadsheet saved
//! with a European locale reads the same. Fields follow RFC 4180: quoted
//! with `"`, a quote doubled inside one, a line break allowed inside one.

use super::{non_empty, normalize_isrc, parse_duration, ImportEntry, ParseError, ParsedPlaylist};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Title,
    Artist,
    Album,
    DurationMs,
    Duration,
    Isrc,
}

fn column_of(header: &str) -> Option<Column> {
    let name = header
        .trim()
        .trim_matches('\u{feff}')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    Some(match name.as_str() {
        "title" | "track" | "track name" | "track title" | "name" | "song" | "song name" => {
            Column::Title
        }
        "artist" | "artists" | "artist name" | "artist name(s)" | "artist names" | "artist(s)" => {
            Column::Artist
        }
        "album" | "album name" | "album title" | "release" => Column::Album,
        "duration_ms" | "duration (ms)" | "duration ms" => Column::DurationMs,
        "duration" | "length" | "time" | "duration (s)" => Column::Duration,
        "isrc" => Column::Isrc,
        _ => return None,
    })
}

/// The separator the header line uses most, outside quotes.
fn detect_separator(text: &str) -> char {
    let header = text.lines().next().unwrap_or("");
    let mut counts = [(',', 0usize), (';', 0), ('\t', 0)];
    let mut quoted = false;
    for c in header.chars() {
        if c == '"' {
            quoted = !quoted;
        } else if !quoted {
            for (sep, count) in counts.iter_mut() {
                if c == *sep {
                    *count += 1;
                }
            }
        }
    }
    // Ties keep the comma: it is first in the list, and `max_by_key`
    // returns the last maximum, so the list is walked in reverse.
    counts
        .iter()
        .rev()
        .max_by_key(|(_, count)| *count)
        .map(|(sep, _)| *sep)
        .unwrap_or(',')
}

/// Split `text` into records of fields (RFC 4180).
fn records(text: &str, sep: char) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.trim().is_empty() => {
                field.clear();
                quoted = true;
            }
            c if c == sep => record.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                record.push(std::mem::take(&mut field));
                out.push(std::mem::take(&mut record));
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        out.push(record);
    }
    // A blank line is one empty field, not a record.
    out.retain(|r| !(r.len() == 1 && r[0].trim().is_empty()));
    out
}

/// Read a CSV playlist. Rows without a title are skipped; the file's name
/// is the caller's to offer as the playlist's, a CSV having none of its own.
pub fn parse(text: &str) -> Result<ParsedPlaylist, ParseError> {
    let text = text.trim_start_matches('\u{feff}');
    let sep = detect_separator(text);
    let mut rows = records(text, sep).into_iter();
    let header = rows.next().ok_or(ParseError::Empty)?;
    let columns: Vec<Option<Column>> = header.iter().map(|h| column_of(h)).collect();
    if !columns.contains(&Some(Column::Title)) {
        return Err(ParseError::NoTitleColumn);
    }
    // The first column of a kind wins: an export with both "Track Name"
    // and "Name" (an album's) means the first.
    let index_of = |wanted: Column| columns.iter().position(|c| *c == Some(wanted));
    let title_at = index_of(Column::Title);
    let artist_at = index_of(Column::Artist);
    let album_at = index_of(Column::Album);
    let duration_ms_at = index_of(Column::DurationMs);
    let duration_at = index_of(Column::Duration);
    let isrc_at = index_of(Column::Isrc);

    let mut entries = Vec::new();
    for row in rows {
        let field = |at: Option<usize>| at.and_then(|i| row.get(i)).map(String::as_str);
        let Some(title) = field(title_at).and_then(non_empty) else {
            continue;
        };
        let duration_ms = field(duration_ms_at)
            .and_then(|raw| parse_duration(raw, true))
            .or_else(|| field(duration_at).and_then(|raw| parse_duration(raw, false)));
        entries.push(ImportEntry {
            title,
            artist: field(artist_at).and_then(non_empty),
            album: field(album_at).and_then(non_empty),
            duration_ms,
            isrc: field(isrc_at).and_then(normalize_isrc),
            locator: None,
        });
        if entries.len() >= super::MAX_ENTRIES {
            break;
        }
    }
    if entries.is_empty() {
        return Err(ParseError::Empty);
    }
    Ok(ParsedPlaylist {
        name: None,
        entries,
    })
}

#[cfg(test)]
mod tests;

//! Which of an episode's transcripts to open, and what it becomes.
//!
//! A timed transcript becomes a [`Document`], which the panel follows line by
//! line as the lyrics panel does. A transcript is not lyrics: it is never
//! looked up elsewhere or written to a file, so nothing here touches the lyrics
//! providers or [`crate::lyrics::Format`].

use serde::{Deserialize, Serialize};

use super::MAX_TRANSCRIPT_BYTES;
use crate::lyrics::{srt, transcript_json, vtt, Document, Line, ParseError};
use crate::markup;

/// How many lines an untimed transcript keeps.
const MAX_TEXT_LINES: usize = 50_000;

/// The longest line of an untimed transcript kept, in characters.
const MAX_TEXT_LINE_CHARS: usize = 4_000;

/// The formats a `podcast:transcript` may be in, as WaveFlow reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptKind {
    Vtt,
    Srt,
    TranscriptJson,
    Text,
    Html,
}

/// One `podcast:transcript` of an episode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptRef {
    pub url: String,
    pub kind: TranscriptKind,
    pub language: Option<String>,
}

impl TranscriptKind {
    /// What a declared type says it is. Without one, the address's extension.
    /// A type nobody here can read names no kind, and that transcript is not
    /// offered.
    pub fn recognise(declared: Option<&str>, url: &str) -> Option<Self> {
        match declared {
            Some(declared) => Self::from_mime(declared),
            None => Self::from_path(url),
        }
    }

    fn from_mime(declared: &str) -> Option<Self> {
        let mime = declared.split(';').next()?.trim().to_ascii_lowercase();
        match mime.as_str() {
            "text/vtt" | "text/webvtt" => Some(Self::Vtt),
            "application/x-subrip" | "application/srt" | "text/srt" | "text/x-srt" => {
                Some(Self::Srt)
            }
            "application/json" | "text/json" => Some(Self::TranscriptJson),
            "text/plain" => Some(Self::Text),
            "text/html" | "application/xhtml+xml" => Some(Self::Html),
            _ => None,
        }
    }

    fn from_path(url: &str) -> Option<Self> {
        let parsed = url::Url::parse(url).ok()?;
        let path = parsed.path().to_ascii_lowercase();
        let extension = path.rsplit('.').next()?;
        match extension {
            "vtt" => Some(Self::Vtt),
            "srt" => Some(Self::Srt),
            "json" => Some(Self::TranscriptJson),
            "txt" => Some(Self::Text),
            "html" | "htm" => Some(Self::Html),
            _ => None,
        }
    }

    /// Lower is better: WebVTT, then SRT, then Podcasting 2.0 JSON, then plain
    /// text or HTML, which cannot follow the position.
    fn rank(self) -> u8 {
        match self {
            Self::Vtt => 0,
            Self::Srt => 1,
            Self::TranscriptJson => 2,
            Self::Text | Self::Html => 3,
        }
    }

    /// Whether this kind's lines carry times.
    pub fn is_timed(self) -> bool {
        self.rank() < 3
    }
}

/// The transcript to open: the best kind, and the first the feed gave of it.
pub fn choose(transcripts: &[TranscriptRef]) -> Option<&TranscriptRef> {
    transcripts.iter().min_by_key(|t| t.kind.rank())
}

/// A fetched transcript as a document. Refuses more than
/// [`MAX_TRANSCRIPT_BYTES`], and a body that is not what its kind says.
pub fn read(kind: TranscriptKind, bytes: &[u8]) -> Result<Document, ParseError> {
    if bytes.len() > MAX_TRANSCRIPT_BYTES {
        return Err(ParseError::NotTimedText);
    }
    let text = crate::text_encoding::decode(bytes);
    match kind {
        TranscriptKind::Vtt => vtt::parse(&text),
        TranscriptKind::Srt => srt::parse(&text),
        TranscriptKind::TranscriptJson => transcript_json::parse(&text),
        TranscriptKind::Text => Ok(untimed(&text)),
        TranscriptKind::Html => Ok(untimed(&markup::html_to_text(&text))),
    }
}

fn untimed(text: &str) -> Document {
    Document {
        lines: text
            .lines()
            .map(|line| markup::collapse(line, MAX_TEXT_LINE_CHARS))
            .filter(|line| !line.is_empty())
            .take(MAX_TEXT_LINES)
            .map(|line| Line::from_text(&line))
            .collect(),
        ..Document::default()
    }
}

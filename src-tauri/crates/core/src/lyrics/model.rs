//! The lyrics document every reader produces and every writer consumes.
//!
//! One model for all formats, rich enough for the richest one (TTML): a
//! format that cannot carry a field drops it on export, and a reader that
//! never sees one leaves it empty. Every time is optional, because a
//! document is edited while it is still being synced: a line pasted a
//! second ago has no time yet, and the writers decide what an untimed
//! line means for their format.

use serde::{Deserialize, Serialize};

/// A whole lyrics document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    #[serde(default)]
    pub metadata: Metadata,
    /// The singers, in the order the document declares them.
    #[serde(default)]
    pub agents: Vec<Agent>,
    #[serde(default)]
    pub lines: Vec<Line>,
}

/// What the document says about the song itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// BCP 47 tag of the sung language (`en`, `ja`, `pt-BR`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub songwriters: Vec<String>,
}

/// A voice that sings some of the lines: one singer, or several together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    /// The id lines refer to (`v1`, `v2`, …). Unique within a document.
    pub id: String,
    #[serde(default)]
    pub kind: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentKind {
    #[default]
    Person,
    /// Everybody at once. Players keep a group on the lead's side.
    Group,
    Other,
}

impl AgentKind {
    pub fn as_ttml(self) -> &'static str {
        match self {
            AgentKind::Person => "person",
            AgentKind::Group => "group",
            AgentKind::Other => "other",
        }
    }

    pub fn from_ttml(value: &str) -> Self {
        match value {
            "group" => AgentKind::Group,
            "person" => AgentKind::Person,
            _ => AgentKind::Other,
        }
    }
}

/// One lyric line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub begin_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<u64>,
    /// Id of the [`Agent`] singing it; `None` is the lead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Song section (`Verse`, `Chorus`, …), carried by TTML only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    /// The sung text, one entry per timed unit: a word, or a syllable of
    /// one when [`Word::joined`] glues it to the next.
    #[serde(default)]
    pub words: Vec<Word>,
    /// Backing vocals sung over this line, timed on their own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub background: Vec<Word>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub translations: Vec<Translation>,
    /// Latin-script reading of the line (romaji, pinyin, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub romanization: Option<String>,
}

/// One timed unit of a line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Word {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub begin_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<u64>,
    /// No space between this unit and the next: they are syllables of
    /// one word (`beau` + `ti` + `ful`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub joined: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A line's text in another language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Translation {
    /// BCP 47 tag; `None` when the source did not say (an LRC duplicate
    /// line never does).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    pub text: String,
}

impl Word {
    pub fn new(text: impl Into<String>) -> Self {
        Word {
            text: text.into(),
            ..Word::default()
        }
    }

    pub fn timed(text: impl Into<String>, begin_ms: u64, end_ms: Option<u64>) -> Self {
        Word {
            text: text.into(),
            begin_ms: Some(begin_ms),
            end_ms,
            joined: false,
        }
    }
}

/// Split free text into untimed words.
pub fn split_words(text: &str) -> Vec<Word> {
    text.split_whitespace().map(Word::new).collect()
}

/// Join units back into display text, honouring [`Word::joined`].
pub fn join_words(words: &[Word]) -> String {
    let mut out = String::new();
    for (i, word) in words.iter().enumerate() {
        out.push_str(&word.text);
        if !word.joined && i + 1 < words.len() {
            out.push(' ');
        }
    }
    out
}

impl Line {
    /// An untimed line holding `text`.
    pub fn from_text(text: &str) -> Self {
        Line {
            words: split_words(text),
            ..Line::default()
        }
    }

    pub fn text(&self) -> String {
        join_words(&self.words)
    }

    /// When the line starts: its own time, else its first word's.
    pub fn effective_begin(&self) -> Option<u64> {
        self.begin_ms
            .or_else(|| self.words.first().and_then(|w| w.begin_ms))
    }

    /// Every unit carries a start time.
    pub fn is_word_timed(&self) -> bool {
        !self.words.is_empty() && self.words.iter().all(|w| w.begin_ms.is_some())
    }
}

impl Document {
    /// A document of untimed lines, one per non-blank input line.
    pub fn from_plain_text(text: &str) -> Self {
        Document {
            lines: text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(Line::from_text)
                .collect(),
            ..Document::default()
        }
    }

    /// The lines that hold words. A line without any is written by no
    /// format (see [`crate::lyrics::resolve::resolve`]), so it says
    /// nothing about how a document is synced.
    fn sung_lines(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter().filter(|l| !l.words.is_empty())
    }

    /// Every line that holds words has a start time.
    pub fn is_line_synced(&self) -> bool {
        self.sung_lines().next().is_some()
            && self.sung_lines().all(|l| l.effective_begin().is_some())
    }

    /// Every unit of every line that holds words has a start time.
    pub fn is_word_synced(&self) -> bool {
        self.sung_lines().next().is_some() && self.sung_lines().all(Line::is_word_timed)
    }

    pub fn agent(&self, id: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.id == id)
    }
}

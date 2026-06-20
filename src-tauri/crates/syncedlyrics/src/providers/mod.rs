use serde::{Deserialize, Serialize};

pub(crate) mod genius;
pub(crate) mod lrclib;
pub(crate) mod megalobiz;
pub(crate) mod musixmatch;
pub(crate) mod netease;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Musixmatch,
    Lrclib,
    NetEase,
    Megalobiz,
    Genius,
}

impl Provider {
    pub const fn defaults() -> &'static [Provider] {
        &[
            Provider::Musixmatch,
            Provider::Lrclib,
            Provider::NetEase,
            Provider::Megalobiz,
            Provider::Genius,
        ]
    }

    /// Canonical snake_case identifier. Matches the `serde(rename_all =
    /// "snake_case")` shape so the frontend / DB write the exact same
    /// string the JSON serialiser would emit — keeps round-trip stable
    /// without forcing every caller to spin a `serde_json::to_value`.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Provider::Musixmatch => "musixmatch",
            Provider::Lrclib => "lrclib",
            Provider::NetEase => "net_ease",
            Provider::Megalobiz => "megalobiz",
            Provider::Genius => "genius",
        }
    }

    /// Parse the canonical identifier back into a [`Provider`]. Pairs
    /// with [`Self::as_str`] so a string round-tripped through the
    /// frontend (e.g. user-picked provider in `refetch_lyrics`) maps
    /// back to the original variant. Returns `None` for any unknown
    /// id so the host can reject unknown values explicitly.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "musixmatch" => Some(Provider::Musixmatch),
            "lrclib" => Some(Provider::Lrclib),
            "net_ease" => Some(Provider::NetEase),
            "megalobiz" => Some(Provider::Megalobiz),
            "genius" => Some(Provider::Genius),
            _ => None,
        }
    }
}

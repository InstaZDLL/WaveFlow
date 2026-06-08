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
}

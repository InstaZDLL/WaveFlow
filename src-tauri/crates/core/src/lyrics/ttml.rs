//! TTML lyrics, in the shape music services serve them: `ttm:agent`
//! singers, `ttm:role="x-bg"` backing vocals, and translations and
//! transliterations keyed to each line through `itunes:key`.
//!
//! Reading is namespace-tolerant (attributes and elements are matched by
//! local name), because documents in the wild bind the same vocabulary
//! to different prefixes. Writing emits one canonical form.

mod parse;
mod write;

pub use parse::parse;
pub use write::{write, Timing, WriteOptions};

pub(crate) const NS_TT: &str = "http://www.w3.org/ns/ttml";
pub(crate) const NS_TTM: &str = "http://www.w3.org/ns/ttml#metadata";
pub(crate) const NS_ITUNES: &str = "http://music.apple.com/lyric-ttml-internal";

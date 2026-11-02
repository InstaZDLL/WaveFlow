//! What a share has to offer, whatever protocol reaches it.
//!
//! Four read operations and nothing else: the first version of the network
//! library cannot write to a share because no backend can express it.

use futures::future::BoxFuture;

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    /// 0 for a directory.
    pub size: u64,
    /// Milliseconds since the epoch; 0 when the server does not say. A
    /// WebDAV server answers to the second, an SMB one far finer.
    pub modified_ms: i64,
}

/// Why an operation failed, sorted by what the caller does about it.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum NetError {
    /// The process-wide offline switch is on. Nothing was sent.
    #[error("offline mode is enabled")]
    Offline,
    /// No answer: refused, DNS, timeout, a dropped link. May pass.
    #[error("unreachable: {0}")]
    Unreachable(String),
    /// The certificate was refused. Named apart because retrying cannot
    /// help, and the user can only act on it knowing that this is the cause.
    #[error("certificate refused: {0}")]
    Tls(String),
    /// The server refused these credentials, or this account.
    #[error("access refused: {0}")]
    Auth(String),
    #[error("not found")]
    NotFound,
    /// It answered, but not like a share: no WebDAV, no range support.
    #[error("{0}")]
    Protocol(String),
    /// A newer command let go of this read.
    #[error("{}", waveflow_core::playback::read_ahead::READ_RELEASED)]
    Released,
}

impl NetError {
    /// A short stable name the interface turns into a sentence.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Unreachable(_) | Self::Released => "unreachable",
            Self::Tls(_) => "tls",
            Self::Auth(_) => "auth",
            Self::NotFound => "not-found",
            Self::Protocol(_) => "protocol",
        }
    }
}

/// A share. Paths are relative to its root, `/`-separated, never starting
/// with `/`, the empty string being the root itself. Every path handed in has
/// passed [`waveflow_core::network_locator::is_safe_path`], and every name
/// handed back must pass it too.
pub trait NetworkFs: Send + Sync {
    /// Reach the share and sign in.
    fn probe(&self) -> BoxFuture<'_, Result<(), NetError>>;
    /// The entries directly inside `dir`, itself excluded.
    fn list<'a>(&'a self, dir: &'a str) -> BoxFuture<'a, Result<Vec<Entry>, NetError>>;
    fn stat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Entry, NetError>>;
    /// Up to `len` bytes from `offset`. Fewer only at the end of the file.
    fn read_at<'a>(
        &'a self,
        path: &'a str,
        offset: u64,
        len: u64,
    ) -> BoxFuture<'a, Result<Vec<u8>, NetError>>;
}

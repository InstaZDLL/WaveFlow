#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! Pure-Rust SMB2/3 client library with pipelined I/O.
//!
//! No C dependencies, no FFI. Pipelined reads/writes fill the credit window
//! so downloads run ~10-25x faster than sequential SMB clients.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use smb2::{SmbClient, ClientConfig};
//!
//! # async fn example() -> Result<(), smb2::Error> {
//! let mut client = smb2::connect("192.168.1.100:445", "user", "pass").await?;
//!
//! // List shares
//! let shares = client.list_shares().await?;
//!
//! // Connect to a share
//! let mut share = client.connect_share("Documents").await?;
//!
//! // List files
//! let entries = client.list_directory(&mut share, "projects/").await?;
//! for entry in &entries {
//!     println!("{} ({} bytes)", entry.name, entry.size);
//! }
//!
//! // Read a file
//! let data = client.read_file(&mut share, "report.pdf").await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Async runtime
//!
//! Runs on [tokio](https://tokio.rs) (the `tokio` feature, on by default) or
//! [smol](https://github.com/smol-rs/smol) (the `smol` feature). With `smol`
//! on, it also runs under any other executor, since smol brings its own
//! reactor and executor threads. With both on, each call runs on tokio when
//! it's made inside a tokio runtime and on smol otherwise, so feature
//! unification can't put a consumer on the wrong one. A smol-only consumer
//! leaves tokio's reactor and runtime out of the build (the crate still uses
//! tokio's channels and semaphores, which work on any executor):
//!
//! ```toml
//! smb2 = { version = "0.26", default-features = false, features = ["smol"] }
//! ```
//!
//! # Modules
//!
//! - [`client`] -- High-level API: [`SmbClient`], [`Tree`], [`Pipeline`].
//!   This is what most users need.
//! - [`error`] -- Error types and NTSTATUS mapping.
//! - [`msg`] -- Wire format message structs (advanced/internal use).
//! - [`name`] -- The private-use-area mapping that lets a name carrying `?`,
//!   `*`, `"` or another SMB2-illegal character exist on a share at all. Runs
//!   on every path automatically; the functions are public for a consumer that
//!   wants to see the literal wire form.
//! - [`pack`] -- Binary serialization primitives (advanced/internal use).
//! - [`transport`] -- Transport trait and TCP implementation (advanced/internal use).
//! - [`crypto`] -- Signing and encryption (advanced/internal use).
//! - [`auth`] -- NTLM authentication (advanced/internal use).
//! - [`rpc`] -- Named pipe RPC for share enumeration (advanced/internal use).
//! - [`types`] -- Protocol newtypes and flag types (advanced/internal use).

pub mod auth;
pub mod client;
pub mod crypto;
pub mod error;
pub mod msg;
pub mod name;
pub mod pack;
pub mod rpc;
pub(crate) mod rt;
#[cfg(feature = "testing")]
pub mod testing;
pub mod transport;
pub mod types;

#[cfg(feature = "fuzzing")]
pub mod fuzzing;

/// The read-ahead tuning, overridable process-wide so `benchmarks/read-ahead/`
/// can compare candidates. Unstable: behind the `__bench-tuning` feature, not
/// for applications, and not covered by SemVer.
#[cfg(feature = "__bench-tuning")]
#[doc(hidden)]
pub mod __bench {
    pub use crate::client::tuning::{
        set_tuning, Estimator, Headroom, LearnedHeadroom, RateMeasure, Tuning,
    };
}

// ── Re-exports: the simple-case imports ────────────────────────────────

// Error types
pub use error::{Error, ErrorKind, Result};

/// Filename mapping for characters SMB2 does not allow on the wire.
pub use name::{decode_name, decode_path, encode_name, encode_path};

// High-level client
pub use client::{connect, ClientConfig, DfsSignIn, DfsTargetPolicy, SmbClient};

// Streaming I/O
pub use client::stream::{
    FileDownload, FileReader, FileUpload, FileWriter, Progress, ReadAhead, WriteBehind,
    WriteCheckpoint, DOWNLOAD_CHUNK_SIZE, UPLOAD_CHUNK_SIZE,
};

// Server-side copy (FSCTL_SRV_COPYCHUNK): copy byte ranges between two files
// on the server without the data crossing the wire.
pub use client::copy::{
    CopyChunk, CopyChunkOutcome, CopyChunkResult, ResumeKey, ServerSideCopyLimits,
};

// Tree and file types
pub use client::directory::{DirectoryReader, ListOptions};
pub use client::tree::{
    DfsLink, DfsOrigin, DirectoryEntry, FileInfo, FsInfo, ListingTrace, QueryStep, RenameOptions,
    Tree,
};

// Which of a file's timestamps and attributes to set (`Tree::set_times`), and
// the attributes `FileInfo` and `DirectoryEntry` report.
pub use client::times::FileTimes;
pub use types::flags::FileAttributes;

// Named streams, macOS xattrs, and the pipelined calls over them.
pub use client::batch::{BatchOptions, BatchResults};
pub use client::streams::{FsAttributes, StreamInfo};
pub use client::xattr::{
    stream_xattr_name, xattr_stream_name, XATTR_FINDER_INFO, XATTR_RESOURCE_FORK,
};

// What kind of reparse point (junction, symlink, cloud placeholder) an entry is.
pub use types::reparse::ReparseTag;

// What a path resolved to on the server (`Tree::resolve`), and which file it is.
pub use client::durable::FileIdentity;
pub use client::resolve::Resolved;

// Pipeline
pub use client::pipeline::{Op, OpResult, Pipeline};

// Connection-level types (useful for advanced users)
pub use client::connection::{
    CompoundOp, Frame, NegotiatedParams, ReconnectEvent, ReconnectObserver, ReconnectPolicy,
    SessionReviver,
};
pub use client::session::Session;

// Diagnostics: snapshot tree returned by `SmbClient::diagnostics()` /
// `Connection::diagnostics()`.
pub use client::diagnostics::{
    ClientInfo, ClientMetricsSnapshot, CompressionInfo, ConnectionDiagnostics, CreditInfo,
    DfsCacheEntry, Diagnostics, EncryptionInfo, InboundProgress, Liveness, MetricsSnapshot,
    NegotiatedSummary, SessionDiagnostics, SigningInfo,
};

// File watching
pub use client::watcher::{FileNotifyAction, FileNotifyEvent, WatchOptions, Watcher};

// Share enumeration
pub use rpc::srvsvc::ShareInfo;

// Kerberos authentication
pub use auth::kerberos::{KerberosAuthenticator, KerberosCredentials};

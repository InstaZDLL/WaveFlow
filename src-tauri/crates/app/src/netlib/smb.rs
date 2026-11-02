//! An SMB share, over [`smb2`]: pure Rust, SMB 2 and 3 only, with signing and
//! encryption as the server asks.
//!
//! One session per source. A listing or a `stat` takes the session in turn; a
//! read goes through an open file handle that needs nothing of the session once
//! it is open, so the transfer behind playback and a scan's reads do not queue
//! behind each other. The handles of the last few files are kept, because a
//! tag parse, a hash and a transfer each make many reads of one file, and
//! opening it is a round trip every time.
//!
//! A session that died is brought back by the client on its own for what can be
//! replayed safely, which is everything here: this backend only reads. When
//! that fails too the session is dropped, and the next call dials again.

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use futures::future::BoxFuture;
use smb2::client::stream::FileReader;
use smb2::{ClientConfig, ErrorKind, SmbClient, Tree};
use tokio::sync::Mutex;
use waveflow_core::network_locator::is_safe_path;

use super::fs::{Entry, NetError, NetworkFs};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_PORT: u16 = 445;

/// Open handles kept: the track playing, the next, and a scan's four.
const MAX_READERS: usize = 8;

struct Session {
    client: SmbClient,
    tree: Tree,
}

pub struct SmbFs {
    config: ClientConfig,
    share: String,
    session: Mutex<Option<Session>>,
    /// Most recently used last.
    readers: Mutex<Vec<(String, Arc<FileReader>)>>,
}

fn classify(err: &smb2::Error) -> NetError {
    match err.kind() {
        ErrorKind::NotFound | ErrorKind::IsADirectory | ErrorKind::NotADirectory => {
            NetError::NotFound
        }
        ErrorKind::AuthRequired | ErrorKind::AccessDenied | ErrorKind::SigningRequired => {
            NetError::Auth(err.to_string())
        }
        ErrorKind::GuestSessionRefused => NetError::Auth("wrong user name or password".to_string()),
        ErrorKind::Io
        | ErrorKind::ConnectionLost
        | ErrorKind::TimedOut
        | ErrorKind::SessionExpired
        | ErrorKind::DfsTargetUnreachable => NetError::Unreachable(err.to_string()),
        _ => NetError::Protocol(err.to_string()),
    }
}

/// `host` or `host:port`, the default port spelled out as the client wants it.
/// An IPv6 literal is written `[::1]:445`.
pub fn normalise_endpoint(endpoint: &str) -> Result<String, NetError> {
    let endpoint = endpoint.trim().trim_start_matches("smb://");
    let endpoint = endpoint.trim_matches(['/', '\\']);
    if endpoint.is_empty() || endpoint.contains(['/', '\\', '@', ' ']) {
        return Err(NetError::Protocol(
            "the address is a host name or an IP address, with the share in its own field".into(),
        ));
    }
    let has_port = match endpoint.rsplit_once(':') {
        // `]` closes an IPv6 literal: `[::1]` has no port, `[::1]:445` does.
        Some((host, port)) => {
            (!host.contains(':') || host.ends_with(']')) && port.parse::<u16>().is_ok()
        }
        None => false,
    };
    Ok(if has_port {
        endpoint.to_string()
    } else {
        format!("{endpoint}:{DEFAULT_PORT}")
    })
}

impl SmbFs {
    pub fn new(
        endpoint: &str,
        share: &str,
        username: Option<String>,
        password: Option<String>,
    ) -> Result<Self, NetError> {
        let share = share.trim().trim_matches(['/', '\\']);
        if share.is_empty() || share.contains(['/', '\\']) {
            return Err(NetError::Protocol(
                "the name of the share is missing".into(),
            ));
        }
        Ok(Self {
            config: ClientConfig {
                addr: normalise_endpoint(endpoint)?,
                timeout: CONNECT_TIMEOUT,
                username: username.unwrap_or_default(),
                password: password.unwrap_or_default(),
                auto_reconnect: true,
                ..Default::default()
            },
            share: share.to_string(),
            session: Mutex::new(None),
            readers: Mutex::new(Vec::new()),
        })
    }

    async fn connect(&self) -> Result<Session, NetError> {
        let mut client = SmbClient::connect(self.config.clone())
            .await
            .map_err(|e| classify(&e))?;
        let tree = client
            .connect_share(&self.share)
            .await
            .map_err(|e| classify(&e))?;
        Ok(Session { client, tree })
    }

    /// Run `operation` on the session, dialing first if there is none, and
    /// dropping it after a failure that says the connection is gone.
    async fn with_session<T>(
        &self,
        operation: impl for<'s> FnOnce(&'s mut Session) -> BoxFuture<'s, Result<T, smb2::Error>>,
    ) -> Result<T, NetError> {
        let mut slot = self.session.lock().await;
        if slot.is_none() {
            *slot = Some(self.connect().await?);
        }
        let session = slot.as_mut().expect("just connected");
        match operation(session).await {
            Ok(value) => Ok(value),
            Err(err) => {
                let classified = classify(&err);
                if matches!(classified, NetError::Unreachable(_)) {
                    *slot = None;
                    self.readers.lock().await.clear();
                }
                Err(classified)
            }
        }
    }

    async fn reader(&self, path: &str) -> Result<Arc<FileReader>, NetError> {
        {
            let mut readers = self.readers.lock().await;
            if let Some(at) = readers.iter().position(|(held, _)| held == path) {
                let entry = readers.remove(at);
                let reader = Arc::clone(&entry.1);
                readers.push(entry);
                return Ok(reader);
            }
        }
        let owned = path.to_string();
        let reader = self
            .with_session(move |session| {
                Box::pin(
                    async move { session.client.open_file_reader(&session.tree, &owned).await },
                )
            })
            .await?;
        let reader = Arc::new(reader);
        let mut readers = self.readers.lock().await;
        // Dropped rather than closed: a read may still hold it, and the server
        // closes what is left when the session ends.
        if readers.len() >= MAX_READERS {
            readers.remove(0);
        }
        readers.push((path.to_string(), Arc::clone(&reader)));
        Ok(reader)
    }
}

fn millis(time: smb2::pack::FileTime) -> i64 {
    time.to_system_time()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

impl NetworkFs for SmbFs {
    fn probe(&self) -> BoxFuture<'_, Result<(), NetError>> {
        Box::pin(async move { self.with_session(|_| Box::pin(async { Ok(()) })).await })
    }

    fn list<'a>(&'a self, dir: &'a str) -> BoxFuture<'a, Result<Vec<Entry>, NetError>> {
        Box::pin(async move {
            let owned = dir.to_string();
            let entries = self
                .with_session(move |session| {
                    Box::pin(async move {
                        session
                            .client
                            .list_directory(&mut session.tree, &owned)
                            .await
                    })
                })
                .await?;
            Ok(entries
                .into_iter()
                // A link can lead out of the share, or back into a parent.
                .filter(|entry| !entry.is_link())
                .filter(|entry| {
                    entry.name != "."
                        && entry.name != ".."
                        && is_safe_path(&entry.name)
                        && !entry.name.contains('/')
                })
                .map(|entry| Entry {
                    is_dir: entry.is_directory,
                    size: if entry.is_directory { 0 } else { entry.size },
                    modified_ms: millis(entry.modified),
                    name: entry.name,
                })
                .collect())
        })
    }

    fn stat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Entry, NetError>> {
        Box::pin(async move {
            let owned = path.to_string();
            let info = self
                .with_session(move |session| {
                    Box::pin(async move { session.client.stat(&mut session.tree, &owned).await })
                })
                .await?;
            Ok(Entry {
                name: path.rsplit('/').next().unwrap_or("").to_string(),
                is_dir: info.is_directory,
                size: info.size,
                modified_ms: millis(info.modified),
            })
        })
    }

    fn read_at<'a>(
        &'a self,
        path: &'a str,
        offset: u64,
        len: u64,
    ) -> BoxFuture<'a, Result<Vec<u8>, NetError>> {
        Box::pin(async move {
            let reader = self.reader(path).await?;
            match reader.read_at(offset, len).await {
                Ok(bytes) => Ok(bytes),
                Err(err) => {
                    let classified = classify(&err);
                    // A handle can outlive the session behind it. The next read
                    // opens it afresh, and the caller decides about this one.
                    self.readers.lock().await.retain(|(held, _)| held != path);
                    if matches!(classified, NetError::Unreachable(_)) {
                        *self.session.lock().await = None;
                    }
                    Err(classified)
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_gets_its_port_and_loses_nothing() {
        let cases = [
            ("nas.local", "nas.local:445"),
            ("nas.local:1445", "nas.local:1445"),
            ("smb://nas.local/", "nas.local:445"),
            ("192.168.1.10", "192.168.1.10:445"),
            ("[::1]", "[::1]:445"),
            ("[::1]:1445", "[::1]:1445"),
        ];
        for (input, expected) in cases {
            assert_eq!(normalise_endpoint(input).unwrap(), expected, "{input}");
        }
    }

    #[test]
    fn an_address_with_a_share_or_credentials_in_it_is_refused() {
        for input in ["", "nas.local/music", r"\\nas\music", "me:secret@nas.local"] {
            assert!(normalise_endpoint(input).is_err(), "{input}");
        }
    }

    #[test]
    fn a_share_needs_a_name_of_one_segment() {
        assert!(SmbFs::new("nas.local", "", None, None).is_err());
        assert!(SmbFs::new("nas.local", "a/b", None, None).is_err());
        assert!(SmbFs::new("nas.local", "/music/", None, None).is_ok());
    }
}

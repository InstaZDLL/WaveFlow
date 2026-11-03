//! Docker-based SMB test servers for integration testing.
//!
//! Provides [`TestServers`] for starting Samba containers on demand,
//! with factory methods that return connected [`SmbClient`] instances.
//! Enable the `testing` feature flag to use this module.
//!
//! # Three-layer testing model
//!
//! **Layer 1: Rust integration tests** -- Use [`TestServers`] to get
//! pre-connected clients in `#[tokio::test]` functions.
//!
//! **Layer 2: E2E tests** -- Use [`write_compose_files`] to extract
//! embedded Docker infrastructure, then run `docker compose up` from
//! your test framework (Playwright, Cypress, etc.).
//!
//! **Layer 3: Manual QA** -- Extract compose files once, run containers
//! manually, browse virtual servers in your app during development.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::LazyLock;
//! use smb2::testing::TestServers;
//!
//! static SERVERS: LazyLock<TestServers> = LazyLock::new(|| {
//!     TestServers::start_blocking().unwrap()
//! });
//!
//! # async fn example() {
//! let mut guest = SERVERS.guest_client().await.unwrap();
//! let shares = guest.list_shares().await.unwrap();
//! # }
//! ```

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use log::{debug, info};

use crate::client::{ClientConfig, SmbClient};

// ── Error type ──────────────────────────────────────────────────────────

/// Errors from the test infrastructure (Docker, process, health checks).
///
/// Separate from [`crate::Error`] because these are test-setup failures,
/// not protocol errors.
#[derive(Debug)]
pub enum Error {
    /// Docker compose command failed.
    Docker(std::io::Error),
    /// Container didn't pass health check in time.
    HealthCheckTimeout {
        /// Name of the container that timed out.
        container: String,
    },
    /// Requested a client for a container that isn't running.
    ContainerNotStarted {
        /// Name of the container that was requested.
        container: String,
        /// Suggestion for how to fix this.
        hint: String,
    },
    /// SMB connection or operation failed.
    Smb(crate::Error),
    /// Failed to write embedded files to disk.
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Docker(e) => write!(f, "docker command failed: {e}"),
            Error::HealthCheckTimeout { container } => {
                write!(f, "health check timed out for container: {container}")
            }
            Error::ContainerNotStarted { container, hint } => {
                write!(f, "container not started: {container} ({hint})")
            }
            Error::Smb(e) => write!(f, "smb connection failed: {e}"),
            Error::Io(e) => write!(f, "failed to write compose files: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Docker(e) | Error::Io(e) => Some(e),
            Error::Smb(e) => Some(e),
            _ => None,
        }
    }
}

/// Result type for test infrastructure operations.
pub type Result<T> = std::result::Result<T, Error>;

// ── Port constants ──────────────────────────────────────────────────────

const DEFAULT_GUEST_PORT: u16 = 10480;
const DEFAULT_AUTH_PORT: u16 = 10481;
const DEFAULT_BOTH_PORT: u16 = 10482;
const DEFAULT_50SHARES_PORT: u16 = 10483;
const DEFAULT_UNICODE_PORT: u16 = 10484;
const DEFAULT_LONGNAMES_PORT: u16 = 10485;
const DEFAULT_DEEPNEST_PORT: u16 = 10486;
const DEFAULT_MANYFILES_PORT: u16 = 10487;
const DEFAULT_READONLY_PORT: u16 = 10488;
const DEFAULT_WINDOWS_PORT: u16 = 10489;
const DEFAULT_SYNOLOGY_PORT: u16 = 10490;
const DEFAULT_LINUX_PORT: u16 = 10491;
const DEFAULT_FLAKY_PORT: u16 = 10492;
const DEFAULT_SLOW_PORT: u16 = 10493;
const DEFAULT_MAXREADSIZE_PORT: u16 = 10494;
const DEFAULT_FRUIT_PORT: u16 = 10495;

/// Resolve a port from an environment variable, falling back to a default.
fn port(env_var: &str, default: u16) -> u16 {
    std::env::var(env_var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Port for the guest-access container.
pub fn guest_port() -> u16 {
    port("SMB_CONSUMER_GUEST_PORT", DEFAULT_GUEST_PORT)
}

/// Port for the auth-required container.
pub fn auth_port() -> u16 {
    port("SMB_CONSUMER_AUTH_PORT", DEFAULT_AUTH_PORT)
}

/// Port for the mixed auth container.
pub fn both_port() -> u16 {
    port("SMB_CONSUMER_BOTH_PORT", DEFAULT_BOTH_PORT)
}

/// Port for the 50-shares container.
pub fn many_shares_port() -> u16 {
    port("SMB_CONSUMER_50SHARES_PORT", DEFAULT_50SHARES_PORT)
}

/// Port for the unicode container.
pub fn unicode_port() -> u16 {
    port("SMB_CONSUMER_UNICODE_PORT", DEFAULT_UNICODE_PORT)
}

/// Port for the long-names container.
pub fn longnames_port() -> u16 {
    port("SMB_CONSUMER_LONGNAMES_PORT", DEFAULT_LONGNAMES_PORT)
}

/// Port for the deep-nesting container.
pub fn deepnest_port() -> u16 {
    port("SMB_CONSUMER_DEEPNEST_PORT", DEFAULT_DEEPNEST_PORT)
}

/// Port for the many-files container.
pub fn manyfiles_port() -> u16 {
    port("SMB_CONSUMER_MANYFILES_PORT", DEFAULT_MANYFILES_PORT)
}

/// Port for the read-only container.
pub fn readonly_port() -> u16 {
    port("SMB_CONSUMER_READONLY_PORT", DEFAULT_READONLY_PORT)
}

/// Port for the Windows-like container.
pub fn windows_port() -> u16 {
    port("SMB_CONSUMER_WINDOWS_PORT", DEFAULT_WINDOWS_PORT)
}

/// Port for the Synology-like container.
pub fn synology_port() -> u16 {
    port("SMB_CONSUMER_SYNOLOGY_PORT", DEFAULT_SYNOLOGY_PORT)
}

/// Port for the Linux container.
pub fn linux_port() -> u16 {
    port("SMB_CONSUMER_LINUX_PORT", DEFAULT_LINUX_PORT)
}

/// Port for the flaky container.
pub fn flaky_port() -> u16 {
    port("SMB_CONSUMER_FLAKY_PORT", DEFAULT_FLAKY_PORT)
}

/// Port for the slow container.
pub fn slow_port() -> u16 {
    port("SMB_CONSUMER_SLOW_PORT", DEFAULT_SLOW_PORT)
}

/// Port for the max-read-size container.
///
/// The server enforces `smb2 max read = 65536` and `smb2 max write = 65536`,
/// so every transfer larger than 64 KB is chunked. Consumers can target this
/// fixture to exercise the streaming write/read fallback paths without
/// needing the internal-fixture `smb-maxreadsize` container.
pub fn maxreadsize_port() -> u16 {
    port("SMB_CONSUMER_MAXREADSIZE_PORT", DEFAULT_MAXREADSIZE_PORT)
}

/// Port for `smb-consumer-fruit`, the server with named streams and macOS's
/// `vfs_fruit`. Overridden by `SMB_CONSUMER_FRUIT_PORT`.
pub fn fruit_port() -> u16 {
    port("SMB_CONSUMER_FRUIT_PORT", DEFAULT_FRUIT_PORT)
}

// ── Embedded files ──────────────────────────────────────────────────────

// docker-compose.yml
const COMPOSE_YML: &str = include_str!("fixtures/consumer/docker-compose.yml");

// smb-consumer-guest
const GUEST_DOCKERFILE: &str = include_str!("fixtures/consumer/smb-consumer-guest/Dockerfile");
const GUEST_SMB_CONF: &str = include_str!("fixtures/consumer/smb-consumer-guest/smb.conf");

// smb-consumer-auth
const AUTH_DOCKERFILE: &str = include_str!("fixtures/consumer/smb-consumer-auth/Dockerfile");
const AUTH_SMB_CONF: &str = include_str!("fixtures/consumer/smb-consumer-auth/smb.conf");

// smb-consumer-both
const BOTH_DOCKERFILE: &str = include_str!("fixtures/consumer/smb-consumer-both/Dockerfile");
const BOTH_SMB_CONF: &str = include_str!("fixtures/consumer/smb-consumer-both/smb.conf");

// smb-consumer-50shares
const SHARES50_DOCKERFILE: &str =
    include_str!("fixtures/consumer/smb-consumer-50shares/Dockerfile");
const SHARES50_SMB_CONF: &str = include_str!("fixtures/consumer/smb-consumer-50shares/smb.conf");
const SHARES50_GENERATE_CONF: &str =
    include_str!("fixtures/consumer/smb-consumer-50shares/generate-conf.sh");

// smb-consumer-unicode
const UNICODE_DOCKERFILE: &str = include_str!("fixtures/consumer/smb-consumer-unicode/Dockerfile");
const UNICODE_SMB_CONF: &str = include_str!("fixtures/consumer/smb-consumer-unicode/smb.conf");
const UNICODE_POPULATE: &str = include_str!("fixtures/consumer/smb-consumer-unicode/populate.sh");

// smb-consumer-longnames
const LONGNAMES_DOCKERFILE: &str =
    include_str!("fixtures/consumer/smb-consumer-longnames/Dockerfile");
const LONGNAMES_SMB_CONF: &str = include_str!("fixtures/consumer/smb-consumer-longnames/smb.conf");
const LONGNAMES_POPULATE: &str =
    include_str!("fixtures/consumer/smb-consumer-longnames/populate.sh");

// smb-consumer-deepnest
const DEEPNEST_DOCKERFILE: &str =
    include_str!("fixtures/consumer/smb-consumer-deepnest/Dockerfile");
const DEEPNEST_SMB_CONF: &str = include_str!("fixtures/consumer/smb-consumer-deepnest/smb.conf");
const DEEPNEST_POPULATE: &str = include_str!("fixtures/consumer/smb-consumer-deepnest/populate.sh");

// smb-consumer-manyfiles
const MANYFILES_DOCKERFILE: &str =
    include_str!("fixtures/consumer/smb-consumer-manyfiles/Dockerfile");
const MANYFILES_SMB_CONF: &str = include_str!("fixtures/consumer/smb-consumer-manyfiles/smb.conf");

// smb-consumer-readonly
const READONLY_DOCKERFILE: &str =
    include_str!("fixtures/consumer/smb-consumer-readonly/Dockerfile");
const READONLY_SMB_CONF: &str = include_str!("fixtures/consumer/smb-consumer-readonly/smb.conf");

// smb-consumer-windows
const WINDOWS_DOCKERFILE: &str = include_str!("fixtures/consumer/smb-consumer-windows/Dockerfile");
const WINDOWS_SMB_CONF: &str = include_str!("fixtures/consumer/smb-consumer-windows/smb.conf");

// smb-consumer-synology
const SYNOLOGY_DOCKERFILE: &str =
    include_str!("fixtures/consumer/smb-consumer-synology/Dockerfile");
const SYNOLOGY_SMB_CONF: &str = include_str!("fixtures/consumer/smb-consumer-synology/smb.conf");

// smb-consumer-linux
const LINUX_DOCKERFILE: &str = include_str!("fixtures/consumer/smb-consumer-linux/Dockerfile");
const LINUX_SMB_CONF: &str = include_str!("fixtures/consumer/smb-consumer-linux/smb.conf");

// smb-consumer-flaky
const FLAKY_DOCKERFILE: &str = include_str!("fixtures/consumer/smb-consumer-flaky/Dockerfile");
const FLAKY_SMB_CONF: &str = include_str!("fixtures/consumer/smb-consumer-flaky/smb.conf");
const FLAKY_CYCLE: &str = include_str!("fixtures/consumer/smb-consumer-flaky/cycle.sh");

// smb-consumer-slow
const SLOW_DOCKERFILE: &str = include_str!("fixtures/consumer/smb-consumer-slow/Dockerfile");
const SLOW_SMB_CONF: &str = include_str!("fixtures/consumer/smb-consumer-slow/smb.conf");
const SLOW_ENTRYPOINT: &str = include_str!("fixtures/consumer/smb-consumer-slow/entrypoint.sh");

// smb-consumer-maxreadsize
const MAXREADSIZE_DOCKERFILE: &str =
    include_str!("fixtures/consumer/smb-consumer-maxreadsize/Dockerfile");
const MAXREADSIZE_SMB_CONF: &str =
    include_str!("fixtures/consumer/smb-consumer-maxreadsize/smb.conf");

// smb-consumer-fruit
const FRUIT_DOCKERFILE: &str = include_str!("fixtures/consumer/smb-consumer-fruit/Dockerfile");
const FRUIT_SMB_CONF: &str = include_str!("fixtures/consumer/smb-consumer-fruit/smb.conf");

// ── Embedded file manifest ──────────────────────────────────────────────

/// A file to write into the compose directory.
struct EmbeddedFile {
    /// Path relative to the compose directory root.
    relative_path: &'static str,
    /// File contents.
    contents: &'static str,
    /// Whether the file should be executable (shell scripts). Only Unix has the mode bit to set.
    #[cfg_attr(not(unix), allow(dead_code))]
    executable: bool,
}

/// All files needed to reproduce the consumer Docker infrastructure.
fn embedded_files() -> Vec<EmbeddedFile> {
    vec![
        EmbeddedFile {
            relative_path: "docker-compose.yml",
            contents: COMPOSE_YML,
            executable: false,
        },
        // guest
        EmbeddedFile {
            relative_path: "smb-consumer-guest/Dockerfile",
            contents: GUEST_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-guest/smb.conf",
            contents: GUEST_SMB_CONF,
            executable: false,
        },
        // auth
        EmbeddedFile {
            relative_path: "smb-consumer-auth/Dockerfile",
            contents: AUTH_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-auth/smb.conf",
            contents: AUTH_SMB_CONF,
            executable: false,
        },
        // both
        EmbeddedFile {
            relative_path: "smb-consumer-both/Dockerfile",
            contents: BOTH_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-both/smb.conf",
            contents: BOTH_SMB_CONF,
            executable: false,
        },
        // 50shares
        EmbeddedFile {
            relative_path: "smb-consumer-50shares/Dockerfile",
            contents: SHARES50_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-50shares/smb.conf",
            contents: SHARES50_SMB_CONF,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-50shares/generate-conf.sh",
            contents: SHARES50_GENERATE_CONF,
            executable: true,
        },
        // unicode
        EmbeddedFile {
            relative_path: "smb-consumer-unicode/Dockerfile",
            contents: UNICODE_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-unicode/smb.conf",
            contents: UNICODE_SMB_CONF,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-unicode/populate.sh",
            contents: UNICODE_POPULATE,
            executable: true,
        },
        // longnames
        EmbeddedFile {
            relative_path: "smb-consumer-longnames/Dockerfile",
            contents: LONGNAMES_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-longnames/smb.conf",
            contents: LONGNAMES_SMB_CONF,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-longnames/populate.sh",
            contents: LONGNAMES_POPULATE,
            executable: true,
        },
        // deepnest
        EmbeddedFile {
            relative_path: "smb-consumer-deepnest/Dockerfile",
            contents: DEEPNEST_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-deepnest/smb.conf",
            contents: DEEPNEST_SMB_CONF,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-deepnest/populate.sh",
            contents: DEEPNEST_POPULATE,
            executable: true,
        },
        // manyfiles
        EmbeddedFile {
            relative_path: "smb-consumer-manyfiles/Dockerfile",
            contents: MANYFILES_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-manyfiles/smb.conf",
            contents: MANYFILES_SMB_CONF,
            executable: false,
        },
        // readonly
        EmbeddedFile {
            relative_path: "smb-consumer-readonly/Dockerfile",
            contents: READONLY_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-readonly/smb.conf",
            contents: READONLY_SMB_CONF,
            executable: false,
        },
        // windows
        EmbeddedFile {
            relative_path: "smb-consumer-windows/Dockerfile",
            contents: WINDOWS_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-windows/smb.conf",
            contents: WINDOWS_SMB_CONF,
            executable: false,
        },
        // synology
        EmbeddedFile {
            relative_path: "smb-consumer-synology/Dockerfile",
            contents: SYNOLOGY_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-synology/smb.conf",
            contents: SYNOLOGY_SMB_CONF,
            executable: false,
        },
        // linux
        EmbeddedFile {
            relative_path: "smb-consumer-linux/Dockerfile",
            contents: LINUX_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-linux/smb.conf",
            contents: LINUX_SMB_CONF,
            executable: false,
        },
        // flaky
        EmbeddedFile {
            relative_path: "smb-consumer-flaky/Dockerfile",
            contents: FLAKY_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-flaky/smb.conf",
            contents: FLAKY_SMB_CONF,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-flaky/cycle.sh",
            contents: FLAKY_CYCLE,
            executable: true,
        },
        // slow
        EmbeddedFile {
            relative_path: "smb-consumer-slow/Dockerfile",
            contents: SLOW_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-slow/smb.conf",
            contents: SLOW_SMB_CONF,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-slow/entrypoint.sh",
            contents: SLOW_ENTRYPOINT,
            executable: true,
        },
        // fruit
        EmbeddedFile {
            relative_path: "smb-consumer-fruit/Dockerfile",
            contents: FRUIT_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-fruit/smb.conf",
            contents: FRUIT_SMB_CONF,
            executable: false,
        },
        // maxreadsize
        EmbeddedFile {
            relative_path: "smb-consumer-maxreadsize/Dockerfile",
            contents: MAXREADSIZE_DOCKERFILE,
            executable: false,
        },
        EmbeddedFile {
            relative_path: "smb-consumer-maxreadsize/smb.conf",
            contents: MAXREADSIZE_SMB_CONF,
            executable: false,
        },
    ]
}

// ── File writing ────────────────────────────────────────────────────────

/// Write all embedded Docker files to the given directory.
///
/// Creates the directory structure Docker Compose expects:
///
/// ```text
/// <dir>/
///   docker-compose.yml
///   smb-consumer-guest/
///     Dockerfile
///     smb.conf
///   smb-consumer-auth/
///     Dockerfile
///     smb.conf
///   ...
/// ```
///
/// Use this for Layer 2 (E2E tests) or Layer 3 (manual QA) where you
/// run `docker compose up` outside of Rust.
pub fn write_compose_files(dir: &Path) -> Result<()> {
    let files = embedded_files();
    for file in &files {
        let path = dir.join(file.relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        fs::write(&path, file.contents).map_err(Error::Io)?;

        #[cfg(unix)]
        if file.executable {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            fs::set_permissions(&path, perms).map_err(Error::Io)?;
        }
    }
    debug!("wrote {} embedded files dir={dir:?}", files.len());
    Ok(())
}

// ── Profile ─────────────────────────────────────────────────────────────

/// Which containers to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Profile {
    /// Guest + auth only (fast startup).
    Minimal,
    /// All 14 containers.
    All,
}

impl Profile {
    /// Service names for `docker compose up`.
    fn services(self) -> &'static [&'static str] {
        match self {
            Profile::Minimal => &["smb-consumer-guest", "smb-consumer-auth"],
            Profile::All => &[
                "smb-consumer-guest",
                "smb-consumer-auth",
                "smb-consumer-both",
                "smb-consumer-50shares",
                "smb-consumer-unicode",
                "smb-consumer-longnames",
                "smb-consumer-deepnest",
                "smb-consumer-manyfiles",
                "smb-consumer-readonly",
                "smb-consumer-windows",
                "smb-consumer-synology",
                "smb-consumer-linux",
                "smb-consumer-flaky",
                "smb-consumer-slow",
                "smb-consumer-maxreadsize",
                "smb-consumer-fruit",
            ],
        }
    }
}

// ── TestServers ─────────────────────────────────────────────────────────

/// Docker-based SMB test servers for integration testing.
///
/// Starts Samba containers on construction, stops on drop. Each server
/// type has a factory method returning a connected [`SmbClient`].
///
/// Consumers can also skip `TestServers` entirely and use the compose
/// files directly for E2E or manual testing via [`write_compose_files`].
pub struct TestServers {
    compose_dir: PathBuf,
    profile: Profile,
}

impl TestServers {
    /// Start the minimal set: guest + auth containers.
    ///
    /// This is the fastest option (~2 seconds). Use [`start_all`](Self::start_all)
    /// if you need all 16 containers.
    pub async fn start() -> Result<Self> {
        let servers = Self::prepare(Profile::Minimal)?;
        servers.compose_up()?;
        servers.wait_healthy()?;
        Ok(servers)
    }

    /// Start all 16 consumer containers.
    pub async fn start_all() -> Result<Self> {
        let servers = Self::prepare(Profile::All)?;
        servers.compose_up()?;
        servers.wait_healthy()?;
        Ok(servers)
    }

    /// Blocking version of [`start_all`](Self::start_all) for use in
    /// [`LazyLock`](std::sync::LazyLock) statics.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::LazyLock;
    /// use smb2::testing::TestServers;
    ///
    /// static SERVERS: LazyLock<TestServers> = LazyLock::new(|| {
    ///     TestServers::start_blocking().unwrap()
    /// });
    /// ```
    pub fn start_blocking() -> Result<Self> {
        let servers = Self::prepare(Profile::All)?;
        servers.compose_up()?;
        servers.wait_healthy()?;
        Ok(servers)
    }

    /// Guest-access server. No credentials needed.
    pub async fn guest_client(&self) -> Result<SmbClient> {
        self.require_service("smb-consumer-guest")?;
        let addr = format!("127.0.0.1:{}", guest_port());
        connect_guest(&addr).await
    }

    /// Auth-required server. Needs username and password.
    pub async fn auth_client(&self, user: &str, pass: &str) -> Result<SmbClient> {
        self.require_service("smb-consumer-auth")?;
        let addr = format!("127.0.0.1:{}", auth_port());
        connect_auth(&addr, user, pass).await
    }

    /// Mixed server, guest connection. Can access the "public" share only.
    pub async fn both_client(&self) -> Result<SmbClient> {
        self.require_service("smb-consumer-both")?;
        let addr = format!("127.0.0.1:{}", both_port());
        connect_guest(&addr).await
    }

    /// Mixed server, authenticated connection. Can access both "public"
    /// and "private" shares.
    pub async fn both_client_auth(&self, user: &str, pass: &str) -> Result<SmbClient> {
        self.require_service("smb-consumer-both")?;
        let addr = format!("127.0.0.1:{}", both_port());
        connect_auth(&addr, user, pass).await
    }

    /// Read-only server. Writes return errors.
    pub async fn readonly_client(&self) -> Result<SmbClient> {
        self.require_service("smb-consumer-readonly")?;
        let addr = format!("127.0.0.1:{}", readonly_port());
        connect_guest(&addr).await
    }

    /// Server with 50 shares for testing share enumeration at scale.
    pub async fn many_shares_client(&self) -> Result<SmbClient> {
        self.require_service("smb-consumer-50shares")?;
        let addr = format!("127.0.0.1:{}", many_shares_port());
        connect_guest(&addr).await
    }

    /// Server with unicode share and file names (CJK, emoji, accented characters).
    pub async fn unicode_client(&self) -> Result<SmbClient> {
        self.require_service("smb-consumer-unicode")?;
        let addr = format!("127.0.0.1:{}", unicode_port());
        connect_guest(&addr).await
    }

    /// Server with 200+ character filenames. Tests path truncation.
    pub async fn longnames_client(&self) -> Result<SmbClient> {
        self.require_service("smb-consumer-longnames")?;
        let addr = format!("127.0.0.1:{}", longnames_port());
        connect_guest(&addr).await
    }

    /// Server with 50-level deep directory tree. Tests navigation overflow.
    pub async fn deepnest_client(&self) -> Result<SmbClient> {
        self.require_service("smb-consumer-deepnest")?;
        let addr = format!("127.0.0.1:{}", deepnest_port());
        connect_guest(&addr).await
    }

    /// Server with 10,000+ files in one directory.
    pub async fn many_files_client(&self) -> Result<SmbClient> {
        self.require_service("smb-consumer-manyfiles")?;
        let addr = format!("127.0.0.1:{}", manyfiles_port());
        connect_guest(&addr).await
    }

    /// Server with named streams and macOS's `vfs_fruit`. Tests Finder tags and
    /// other xattrs (share `public`).
    pub async fn fruit_client(&self) -> Result<SmbClient> {
        self.require_service("smb-consumer-fruit")?;
        let addr = format!("127.0.0.1:{}", fruit_port());
        connect_guest(&addr).await
    }

    /// Windows-like server (server string in smb.conf). Tests OS detection.
    pub async fn windows_client(&self) -> Result<SmbClient> {
        self.require_service("smb-consumer-windows")?;
        let addr = format!("127.0.0.1:{}", windows_port());
        connect_guest(&addr).await
    }

    /// Synology-like server (server string in smb.conf). Tests NAS-specific UI.
    pub async fn synology_client(&self) -> Result<SmbClient> {
        self.require_service("smb-consumer-synology")?;
        let addr = format!("127.0.0.1:{}", synology_port());
        connect_guest(&addr).await
    }

    /// Generic Linux Samba server. Most common real-world server type.
    pub async fn linux_client(&self) -> Result<SmbClient> {
        self.require_service("smb-consumer-linux")?;
        let addr = format!("127.0.0.1:{}", linux_port());
        connect_guest(&addr).await
    }

    /// Flaky server (5 seconds up, 5 seconds down). Tests error recovery UI.
    pub async fn flaky_client(&self) -> Result<SmbClient> {
        self.require_service("smb-consumer-flaky")?;
        let addr = format!("127.0.0.1:{}", flaky_port());
        connect_guest(&addr).await
    }

    /// Slow server (200ms latency). Tests loading states and timeouts.
    pub async fn slow_client(&self) -> Result<SmbClient> {
        self.require_service("smb-consumer-slow")?;
        let addr = format!("127.0.0.1:{}", slow_port());
        connect_guest(&addr).await
    }

    // ── Internal helpers ────────────────────────────────────────────

    /// Create a temp directory, write embedded files, return the struct.
    fn prepare(profile: Profile) -> Result<Self> {
        let compose_dir = std::env::temp_dir().join(format!("smb2-testing-{}", std::process::id()));
        write_compose_files(&compose_dir)?;
        info!("prepared compose files dir={compose_dir:?}");
        Ok(Self {
            compose_dir,
            profile,
        })
    }

    /// Run `docker compose up` for the selected profile.
    fn compose_up(&self) -> Result<()> {
        let services = self.profile.services();
        info!("starting {} container(s)", services.len());

        let mut cmd = Command::new("docker");
        cmd.arg("compose")
            .arg("-f")
            .arg(self.compose_dir.join("docker-compose.yml"))
            .arg("up")
            .arg("-d")
            .arg("--build");
        for svc in services {
            cmd.arg(svc);
        }

        debug!("running: {:?}", cmd);
        let output = cmd.output().map_err(Error::Docker)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            debug!("docker compose up stderr: {stderr}");
            return Err(Error::Docker(std::io::Error::other(format!(
                "docker compose up failed: {stderr}"
            ))));
        }
        Ok(())
    }

    /// Wait for all started containers to pass Docker health checks.
    fn wait_healthy(&self) -> Result<()> {
        let services = self.profile.services();
        let timeout = Duration::from_secs(30);
        let poll_interval = Duration::from_millis(500);
        let start = std::time::Instant::now();

        for service in services {
            // Skip health check for flaky container (it intentionally cycles).
            if *service == "smb-consumer-flaky" {
                debug!("skipping health check for {service} (intentionally flaky)");
                continue;
            }

            loop {
                if start.elapsed() > timeout {
                    return Err(Error::HealthCheckTimeout {
                        container: service.to_string(),
                    });
                }

                let output = Command::new("docker")
                    .arg("compose")
                    .arg("-f")
                    .arg(self.compose_dir.join("docker-compose.yml"))
                    .arg("ps")
                    .arg("--format")
                    .arg("{{.Health}}")
                    .arg(service)
                    .output()
                    .map_err(Error::Docker)?;

                let status = String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .to_lowercase();
                if status.contains("healthy") {
                    debug!("{service} is healthy");
                    break;
                }

                debug!("{service} health: {status:?}, waiting...");
                std::thread::sleep(poll_interval);
            }
        }

        info!("all containers healthy");
        Ok(())
    }

    /// Check that a service is part of the current profile.
    fn require_service(&self, service: &str) -> Result<()> {
        if self.profile.services().contains(&service) {
            Ok(())
        } else {
            Err(Error::ContainerNotStarted {
                container: service.to_string(),
                hint: "call start_all() to start all containers".to_string(),
            })
        }
    }

    /// Run `docker compose down` (best-effort).
    fn compose_down(&self) {
        debug!("stopping containers dir={:?}", self.compose_dir);
        let result = Command::new("docker")
            .arg("compose")
            .arg("-f")
            .arg(self.compose_dir.join("docker-compose.yml"))
            .arg("down")
            .arg("--timeout")
            .arg("5")
            .output();

        match result {
            Ok(output) if output.status.success() => {
                info!("containers stopped");
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                debug!("docker compose down stderr: {stderr}");
            }
            Err(e) => {
                debug!("failed to run docker compose down: {e}");
            }
        }
    }

    /// Clean up the temp directory (best-effort).
    fn cleanup_dir(&self) {
        if self.compose_dir.exists() {
            if let Err(e) = fs::remove_dir_all(&self.compose_dir) {
                debug!("failed to clean up dir={:?}: {e}", self.compose_dir);
            }
        }
    }
}

impl Drop for TestServers {
    fn drop(&mut self) {
        self.compose_down();
        self.cleanup_dir();
    }
}

// ── Connection helpers ──────────────────────────────────────────────────

async fn connect_guest(addr: &str) -> Result<SmbClient> {
    SmbClient::connect(ClientConfig {
        addr: addr.to_string(),
        timeout: Duration::from_secs(10),
        username: String::new(),
        password: String::new(),
        domain: String::new(),
        auto_reconnect: false,
        compression: true,
        dfs_enabled: false,
        dfs_target_overrides: std::collections::HashMap::new(),
        connect_options: None,
        ..Default::default()
    })
    .await
    .map_err(Error::Smb)
}

async fn connect_auth(addr: &str, user: &str, pass: &str) -> Result<SmbClient> {
    SmbClient::connect(ClientConfig {
        addr: addr.to_string(),
        timeout: Duration::from_secs(10),
        username: user.to_string(),
        password: pass.to_string(),
        domain: String::new(),
        auto_reconnect: false,
        compression: true,
        dfs_enabled: false,
        dfs_target_overrides: std::collections::HashMap::new(),
        connect_options: None,
        ..Default::default()
    })
    .await
    .map_err(Error::Smb)
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Port resolution ─────────────────────────────────────────────

    #[test]
    fn port_returns_default_when_env_unset() {
        // Use a unique env var name that won't collide with real env.
        let val = port("SMB2_TEST_NONEXISTENT_PORT_12345", 9999);
        assert_eq!(val, 9999);
    }

    #[test]
    fn port_returns_env_value_when_set() {
        let key = "SMB2_TEST_PORT_OVERRIDE_CHECK";
        std::env::set_var(key, "12345");
        let val = port(key, 9999);
        std::env::remove_var(key);
        assert_eq!(val, 12345);
    }

    #[test]
    fn port_returns_default_for_non_numeric_env() {
        let key = "SMB2_TEST_PORT_BAD_VALUE";
        std::env::set_var(key, "not_a_number");
        let val = port(key, 7777);
        std::env::remove_var(key);
        assert_eq!(val, 7777);
    }

    #[test]
    fn port_returns_default_for_empty_env() {
        let key = "SMB2_TEST_PORT_EMPTY";
        std::env::set_var(key, "");
        let val = port(key, 5555);
        std::env::remove_var(key);
        assert_eq!(val, 5555);
    }

    // ── Default port values ─────────────────────────────────────────

    #[test]
    fn default_ports_are_in_consumer_range() {
        let ports = [
            DEFAULT_GUEST_PORT,
            DEFAULT_AUTH_PORT,
            DEFAULT_BOTH_PORT,
            DEFAULT_50SHARES_PORT,
            DEFAULT_UNICODE_PORT,
            DEFAULT_LONGNAMES_PORT,
            DEFAULT_DEEPNEST_PORT,
            DEFAULT_MANYFILES_PORT,
            DEFAULT_READONLY_PORT,
            DEFAULT_WINDOWS_PORT,
            DEFAULT_SYNOLOGY_PORT,
            DEFAULT_LINUX_PORT,
            DEFAULT_FLAKY_PORT,
            DEFAULT_SLOW_PORT,
            DEFAULT_MAXREADSIZE_PORT,
            DEFAULT_FRUIT_PORT,
        ];
        for p in ports {
            assert!(
                (10480..=10495).contains(&p),
                "port {p} outside expected range 10480-10495"
            );
        }
    }

    #[test]
    fn default_ports_are_unique() {
        let ports = [
            DEFAULT_GUEST_PORT,
            DEFAULT_AUTH_PORT,
            DEFAULT_BOTH_PORT,
            DEFAULT_50SHARES_PORT,
            DEFAULT_UNICODE_PORT,
            DEFAULT_LONGNAMES_PORT,
            DEFAULT_DEEPNEST_PORT,
            DEFAULT_MANYFILES_PORT,
            DEFAULT_READONLY_PORT,
            DEFAULT_WINDOWS_PORT,
            DEFAULT_SYNOLOGY_PORT,
            DEFAULT_LINUX_PORT,
            DEFAULT_FLAKY_PORT,
            DEFAULT_SLOW_PORT,
            DEFAULT_MAXREADSIZE_PORT,
            DEFAULT_FRUIT_PORT,
        ];
        let mut seen = std::collections::HashSet::new();
        for p in ports {
            assert!(seen.insert(p), "duplicate port: {p}");
        }
    }

    // ── Error formatting ────────────────────────────────────────────

    #[test]
    fn error_display_docker() {
        let err = Error::Docker(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "docker not found",
        ));
        let msg = err.to_string();
        assert!(msg.contains("docker command failed"), "got: {msg}");
        assert!(msg.contains("docker not found"), "got: {msg}");
    }

    #[test]
    fn error_display_health_check_timeout() {
        let err = Error::HealthCheckTimeout {
            container: "smb-consumer-guest".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("health check timed out"), "got: {msg}");
        assert!(msg.contains("smb-consumer-guest"), "got: {msg}");
    }

    #[test]
    fn error_display_container_not_started() {
        let err = Error::ContainerNotStarted {
            container: "smb-consumer-unicode".to_string(),
            hint: "call start_all()".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("container not started"), "got: {msg}");
        assert!(msg.contains("smb-consumer-unicode"), "got: {msg}");
        assert!(msg.contains("start_all()"), "got: {msg}");
    }

    #[test]
    fn error_display_io() {
        let err = Error::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "permission denied",
        ));
        let msg = err.to_string();
        assert!(msg.contains("write compose files"), "got: {msg}");
    }

    #[test]
    fn error_debug_is_implemented() {
        let err = Error::HealthCheckTimeout {
            container: "test".to_string(),
        };
        // Just verify Debug doesn't panic.
        let _ = format!("{err:?}");
    }

    // ── write_compose_files ─────────────────────────────────────────

    #[test]
    fn write_compose_files_creates_expected_structure() {
        let dir = std::env::temp_dir().join(format!("smb2-test-write-{}", std::process::id()));
        // Clean up from any previous run.
        let _ = fs::remove_dir_all(&dir);

        write_compose_files(&dir).unwrap();

        // Verify top-level compose file.
        assert!(dir.join("docker-compose.yml").exists());

        // Verify all 16 container directories exist with Dockerfiles.
        let containers = [
            "smb-consumer-guest",
            "smb-consumer-auth",
            "smb-consumer-both",
            "smb-consumer-50shares",
            "smb-consumer-unicode",
            "smb-consumer-longnames",
            "smb-consumer-deepnest",
            "smb-consumer-manyfiles",
            "smb-consumer-readonly",
            "smb-consumer-windows",
            "smb-consumer-synology",
            "smb-consumer-linux",
            "smb-consumer-flaky",
            "smb-consumer-slow",
            "smb-consumer-maxreadsize",
            "smb-consumer-fruit",
        ];
        for name in containers {
            let dockerfile = dir.join(name).join("Dockerfile");
            assert!(dockerfile.exists(), "missing Dockerfile for {name}");
            let smb_conf = dir.join(name).join("smb.conf");
            assert!(smb_conf.exists(), "missing smb.conf for {name}");
        }

        // Verify extra scripts exist.
        assert!(dir.join("smb-consumer-50shares/generate-conf.sh").exists());
        assert!(dir.join("smb-consumer-unicode/populate.sh").exists());
        assert!(dir.join("smb-consumer-longnames/populate.sh").exists());
        assert!(dir.join("smb-consumer-deepnest/populate.sh").exists());
        assert!(dir.join("smb-consumer-flaky/cycle.sh").exists());
        assert!(dir.join("smb-consumer-slow/entrypoint.sh").exists());

        // Clean up.
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_compose_files_content_matches_embedded() {
        let dir = std::env::temp_dir().join(format!("smb2-test-content-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        write_compose_files(&dir).unwrap();

        let compose = fs::read_to_string(dir.join("docker-compose.yml")).unwrap();
        assert!(
            compose.contains("smb-consumer-guest"),
            "compose file should reference guest service"
        );
        assert!(
            compose.contains("10480"),
            "compose file should contain default guest port"
        );

        let guest_conf = fs::read_to_string(dir.join("smb-consumer-guest/smb.conf")).unwrap();
        assert!(
            guest_conf.contains("[public]"),
            "guest smb.conf should have [public] share"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn write_compose_files_scripts_are_executable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("smb2-test-exec-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        write_compose_files(&dir).unwrap();

        let scripts = [
            "smb-consumer-50shares/generate-conf.sh",
            "smb-consumer-unicode/populate.sh",
            "smb-consumer-longnames/populate.sh",
            "smb-consumer-deepnest/populate.sh",
            "smb-consumer-flaky/cycle.sh",
            "smb-consumer-slow/entrypoint.sh",
        ];
        for script in scripts {
            let path = dir.join(script);
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert!(
                mode & 0o111 != 0,
                "{script} should be executable (mode: {mode:#o})"
            );
        }

        let _ = fs::remove_dir_all(&dir);
    }

    // ── Profile / require_service ───────────────────────────────────

    #[test]
    fn minimal_profile_includes_guest_and_auth() {
        let services = Profile::Minimal.services();
        assert!(services.contains(&"smb-consumer-guest"));
        assert!(services.contains(&"smb-consumer-auth"));
        assert_eq!(services.len(), 2);
    }

    #[test]
    fn all_profile_includes_16_services() {
        let services = Profile::All.services();
        assert_eq!(services.len(), 16);
    }

    #[test]
    fn require_service_ok_for_minimal_profile() {
        let servers = TestServers {
            compose_dir: PathBuf::from("/tmp/fake"),
            profile: Profile::Minimal,
        };
        assert!(servers.require_service("smb-consumer-guest").is_ok());
        assert!(servers.require_service("smb-consumer-auth").is_ok());
    }

    #[test]
    fn require_service_fails_for_non_minimal_container() {
        let servers = TestServers {
            compose_dir: PathBuf::from("/tmp/fake"),
            profile: Profile::Minimal,
        };
        let err = servers.require_service("smb-consumer-unicode").unwrap_err();
        match err {
            Error::ContainerNotStarted { container, hint } => {
                assert_eq!(container, "smb-consumer-unicode");
                assert!(hint.contains("start_all()"));
            }
            other => panic!("expected ContainerNotStarted, got: {other:?}"),
        }
    }

    #[test]
    fn require_service_ok_for_all_profile() {
        let servers = TestServers {
            compose_dir: PathBuf::from("/tmp/fake"),
            profile: Profile::All,
        };
        // Should succeed for every container.
        for svc in Profile::All.services() {
            assert!(
                servers.require_service(svc).is_ok(),
                "require_service failed for {svc}"
            );
        }
    }

    // ── Embedded file count ─────────────────────────────────────────

    #[test]
    fn embedded_files_count() {
        let files = embedded_files();
        // 1 compose + 16 containers * (Dockerfile + smb.conf) = 33
        // + 6 extra scripts = 39
        assert_eq!(files.len(), 39, "expected 39 embedded files");
    }

    #[test]
    fn embedded_files_no_empty_contents() {
        for file in embedded_files() {
            assert!(
                !file.contents.is_empty(),
                "embedded file {} has empty contents",
                file.relative_path
            );
        }
    }
}

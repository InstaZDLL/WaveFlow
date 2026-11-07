//! In-app auto-updater commands with an opt-in **beta channel**.
//!
//! The Tauri JS `check()` reads its endpoints from the static
//! `tauri.conf.json` block, so it can't switch channels at runtime. To
//! let a user opt into pre-release builds we drive the updater from Rust
//! instead: [`check_for_update`] builds an `UpdaterBuilder` with the
//! endpoint that matches the persisted channel, holds the resulting
//! [`Update`] in managed state, and [`install_update`] consumes it.
//!
//! Channel routing (see `docs/RELEASING.md#beta-channel`):
//!   - **stable** → `releases/latest/download/latest.json`. GitHub's
//!     `/releases/latest` alias *excludes* pre-releases, so a stable
//!     user never sees a beta even though both manifests live in the
//!     same repo.
//!   - **beta** → `releases/download/beta-channel/latest-beta.json`, a
//!     rolling manifest re-uploaded onto the fixed `beta-channel`
//!     release by `release.yml` on every pre-release tag, **and** the
//!     stable manifest, the newer of the two winning. A tester on
//!     1.8.0-beta.3 used to read the beta manifest alone and never saw
//!     1.8.0 (#793).
//!
//! Every manifest has a **mirror** on `download.waveflow.app`, a
//! Cloudflare R2 bucket `release.yml` fills with the same signed
//! payloads. The endpoints are compiled into the binary, so a build
//! that knew only GitHub could never follow the releases anywhere else.
//! Both copies are read and the newest answer wins, which also survives
//! one host being down, answering 404 or serving a page that is not
//! JSON (`docs/RELEASING.md#second-download-source-cloudflare-r2`).
//! Keeping the newest is safe only because `requireSignedVersion` is on
//! in `tauri.conf.json`: a manifest is not signed, and without the
//! version bound into the payload's signature, either host could pair a
//! high version number with an older signed build.
//!
//! A check nobody asked for — the one the frontend makes at launch and
//! after a channel switch — reads no manifest when the user turned
//! `updater.auto_check` off or offline mode is on; a check asked for by
//! hand still runs with the switch off and is refused in offline mode
//! ([`check_for_update`]).
//!
//! The actual updater calls are compiled only into release builds with
//! the `updater` feature (same gate as the plugin registration in
//! `lib.rs`) — `app.updater_builder()` reads `self.state::<UpdaterState>()`
//! which the plugin only manages once registered, so calling it in dev
//! would panic. In every other build the commands degrade to a no-op
//! ("no update available"), matching the pre-existing dev behaviour
//! where the frontend silently swallowed updater errors.

use chrono::Utc;
use sqlx::SqlitePool;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// `app_setting` key holding the chosen release channel. App-wide (not
/// per-profile): an update replaces the whole binary, so the choice
/// can't sensibly differ between profiles on the same machine.
const KEY_CHANNEL: &str = "updater.channel";

/// `app_setting` key holding whether the updater looks for a new version
/// on its own (at launch, and when the channel changes). App-wide for the
/// same reason as the channel. A missing row reads as on.
const KEY_AUTO_CHECK: &str = "updater.auto_check";

/// What [`check_for_update`] answers a check asked for by hand while
/// offline mode is on. The frontend recognises this exact text to say so
/// in words rather than show it as a failure (`src/lib/tauri/updater.ts`).
const OFFLINE_MESSAGE: &str = "updater: offline mode is on";

/// What a build without the updater (dev, package-manager builds) answers
/// a check asked for by hand, and any install. Recognised by the frontend
/// like [`OFFLINE_MESSAGE`].
// Unused when the updater is compiled in.
#[allow(dead_code)]
const UNAVAILABLE_MESSAGE: &str = "updater: unavailable in this build";

/// Stable updater manifest. `/releases/latest/` resolves to the newest
/// non-pre-release, so pre-releases are invisible here by construction.
// Consumed only by the release-gated `imp` module — dead in dev builds.
#[allow(dead_code)]
const STABLE_ENDPOINT: &str =
    "https://github.com/InstaZDLL/WaveFlow/releases/latest/download/latest.json";

/// Rolling beta manifest, re-uploaded onto the fixed `beta-channel`
/// release by `release.yml` on every pre-release tag. Uses a pinned tag
/// (not `/latest/`) because GitHub has no "latest pre-release" alias.
#[allow(dead_code)]
const BETA_ENDPOINT: &str =
    "https://github.com/InstaZDLL/WaveFlow/releases/download/beta-channel/latest-beta.json";

/// The stable manifest's mirror on R2, rewritten by `release.yml` to
/// point at the payloads copied next to it. The signatures are the same
/// bytes, so the updater accepts either copy under the one public key.
#[allow(dead_code)]
const STABLE_MIRROR_ENDPOINT: &str = "https://download.waveflow.app/stable/latest.json";

/// The beta manifest's mirror on R2. Only a pre-release tag writes it:
/// a stable newer than the last beta reaches testers through
/// [`STABLE_MIRROR_ENDPOINT`], which the beta channel reads too, so the
/// copy `release.yml` makes onto `beta-channel` has no mirror twin.
#[allow(dead_code)]
const BETA_MIRROR_ENDPOINT: &str = "https://download.waveflow.app/beta/latest.json";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UpdateChannel {
    Stable,
    Beta,
}

impl UpdateChannel {
    /// Parse a persisted / IPC value. Anything but `"beta"` falls back
    /// to `Stable` so a stray write can never strand a user on betas.
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("beta") => UpdateChannel::Beta,
            _ => UpdateChannel::Stable,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            UpdateChannel::Stable => "stable",
            UpdateChannel::Beta => "beta",
        }
    }

    /// The manifests to read, in order. A beta tester follows stable
    /// releases too: one that is newer than the last beta is an update
    /// for them, and `release.yml` only copies it onto the beta channel
    /// from now on, which a build without this list never learns of.
    ///
    /// Each GitHub manifest is followed by its R2 mirror. They are read
    /// one at a time by `imp::check`, never handed to the plugin as one
    /// list: its own loop (2.14) stops at the first manifest that parses
    /// without comparing versions, and gives up on the spot when a host
    /// answers 200 with a body that is not JSON — a suspended account's
    /// page, a captive portal.
    #[allow(dead_code)] // used only by the release-gated `imp` module
    fn endpoints(self) -> &'static [&'static str] {
        match self {
            UpdateChannel::Stable => &[STABLE_ENDPOINT, STABLE_MIRROR_ENDPOINT],
            UpdateChannel::Beta => &[
                BETA_ENDPOINT,
                BETA_MIRROR_ENDPOINT,
                STABLE_ENDPOINT,
                STABLE_MIRROR_ENDPOINT,
            ],
        }
    }
}

/// Whether `candidate` is a newer version than `current`, by semver
/// (a release outranks its own pre-releases). A version that does not
/// parse never wins, so a malformed manifest cannot displace a good one.
#[allow(dead_code)] // used only by the release-gated `imp` module
fn is_newer(candidate: &str, current: &str) -> bool {
    match (
        semver::Version::parse(candidate),
        semver::Version::parse(current),
    ) {
        // Precedence, not `>`: build metadata (`+…`) does not make a
        // version newer.
        (Ok(candidate), Ok(current)) => candidate.cmp_precedence(&current).is_gt(),
        (Ok(_), Err(_)) => true,
        _ => false,
    }
}

/// Which answers to try installing, and in what order: every one that
/// announces the newest version, in the order the manifests were read.
/// The two hosts serve the same release with the same signatures, so
/// when the first one's payload cannot be downloaded or does not verify,
/// the same version from the other host is still the update — a GitHub
/// that answers its small manifest but not its 100 MB payload is the
/// case. An older version is never a fallback for a newer one.
#[allow(dead_code)] // used only by the release-gated `imp` module
fn install_order(versions: &[&str]) -> Vec<usize> {
    let mut newest: Option<usize> = None;
    for (i, version) in versions.iter().enumerate() {
        if newest.map_or(true, |best| is_newer(version, versions[best])) {
            newest = Some(i);
        }
    }
    let Some(newest) = newest else {
        return Vec::new();
    };
    (0..versions.len())
        .filter(|&i| !is_newer(versions[newest], versions[i]))
        .collect()
}

async fn read_channel(db: &SqlitePool) -> UpdateChannel {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(KEY_CHANNEL)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    UpdateChannel::parse(raw.as_deref())
}

/// Whether automatic checks are on. Only a stored `"false"` / `"0"` turns
/// them off; a missing row or a read that fails keeps the default (on).
async fn read_auto_check(db: &SqlitePool) -> bool {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(KEY_AUTO_CHECK)
        .fetch_optional(db)
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(%err, "could not read updater.auto_check; using the default");
            None
        });
    !matches!(raw.as_deref(), Some("false" | "0"))
}

/// What a check does before any manifest is read.
#[derive(Debug, PartialEq, Eq)]
enum CheckGate {
    /// Read the manifests.
    Read,
    /// Answer "nothing new" without a request: an automatic check while
    /// automatic checks are off, or while offline mode is on.
    Skip,
    /// Refuse with [`OFFLINE_MESSAGE`]: a check asked for by hand while
    /// offline mode is on.
    Offline,
}

fn check_gate(automatic: bool, auto_check_on: bool, offline: bool) -> CheckGate {
    if automatic && (!auto_check_on || offline) {
        CheckGate::Skip
    } else if offline {
        CheckGate::Offline
    } else {
        CheckGate::Read
    }
}

/// Metadata surfaced to the UpdateBanner when a newer build is found.
#[derive(serde::Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: Option<String>,
}

/// Download progress, emitted on `updater:progress` during install.
// Constructed only by the release-gated `imp` module.
#[allow(dead_code)]
#[derive(Clone, serde::Serialize)]
pub struct UpdateProgress {
    pub downloaded: u64,
    /// `0` until the server reports a Content-Length.
    pub total: u64,
}

#[tauri::command]
pub async fn get_update_channel(state: tauri::State<'_, AppState>) -> AppResult<String> {
    Ok(read_channel(&state.app_db).await.as_str().to_string())
}

#[tauri::command]
pub async fn set_update_channel(
    state: tauri::State<'_, AppState>,
    channel: String,
) -> AppResult<()> {
    // Normalise through the enum so only `"stable"` / `"beta"` ever hit
    // the row, regardless of what the caller passed.
    let normalized = UpdateChannel::parse(Some(channel.as_str())).as_str();
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(KEY_CHANNEL)
    .bind(normalized)
    .bind(Utc::now().timestamp_millis())
    .execute(&state.app_db)
    .await?;
    Ok(())
}

#[tauri::command]
pub async fn get_update_auto_check(state: tauri::State<'_, AppState>) -> AppResult<bool> {
    Ok(read_auto_check(&state.app_db).await)
}

#[tauri::command]
pub async fn set_update_auto_check(
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'bool', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(KEY_AUTO_CHECK)
    .bind(if enabled { "true" } else { "false" })
    .bind(Utc::now().timestamp_millis())
    .execute(&state.app_db)
    .await?;
    Ok(())
}

#[cfg(all(not(debug_assertions), feature = "updater"))]
mod imp {
    use super::*;
    use tauri::{AppHandle, Emitter, Manager};
    use tauri_plugin_updater::{Update, UpdaterExt};
    use tokio::sync::Mutex;

    /// Holds the [`Update`]s between [`check_for_update`] and
    /// [`install_update`] — each owns a download URL + signature and can't
    /// cross the IPC boundary, so they live here. Every answer announcing
    /// the newest version, in the order to try them ([`install_order`]);
    /// empty when there is nothing to install.
    #[derive(Default)]
    pub struct PendingUpdate(pub Mutex<Vec<Update>>);

    /// How long one manifest may take to answer.
    const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// How long a download may go without a byte before the next host is
    /// tried. Silence, not the whole download: a slow link still finishes.
    const DOWNLOAD_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// How often a download is looked at for that silence.
    const DOWNLOAD_IDLE_POLL: std::time::Duration = std::time::Duration::from_secs(1);

    /// The cause chain too: the plugin's `Reqwest` variant is transparent,
    /// and its message alone reads "error sending request" — offline, DNS,
    /// refused and timed out all look the same without the layers below.
    fn updater_err(e: impl std::error::Error) -> AppError {
        AppError::Other(format!(
            "updater: {}",
            waveflow_core::error::describe_error_without_urls(&e)
        ))
    }

    async fn check_endpoint(app: &AppHandle, endpoint: &str) -> AppResult<Option<Update>> {
        let endpoint = tauri::Url::parse(endpoint).map_err(updater_err)?;
        let updater = app
            .updater_builder()
            .endpoints(vec![endpoint])
            .map_err(updater_err)?
            .build()
            .map_err(updater_err)?;
        // The plugin sets no timeout of its own, and a host that accepts
        // the connection and never answers would hold back every manifest
        // after it — the mirror is only worth something if it is asked.
        // Around the check alone: the builder's `timeout` would also cap
        // the download of the payload.
        tokio::time::timeout(CHECK_TIMEOUT, updater.check())
            .await
            .map_err(|_| {
                AppError::Other(format!(
                    "updater: no answer within {} s",
                    CHECK_TIMEOUT.as_secs()
                ))
            })?
            .map_err(updater_err)
    }

    pub async fn check(app: &AppHandle, channel: UpdateChannel) -> AppResult<Option<UpdateInfo>> {
        // Each manifest only answers with a version newer than this build,
        // so the newest answer is the update. One manifest failing does not
        // hide what another found; only all of them failing is an error.
        let mut found_updates: Vec<Update> = Vec::new();
        let mut first_err = None;
        let mut any_ok = false;
        for endpoint in channel.endpoints() {
            match check_endpoint(app, endpoint).await {
                Ok(found) => {
                    any_ok = true;
                    found_updates.extend(found);
                }
                Err(err) => {
                    tracing::warn!(%err, endpoint, "update check failed for one manifest");
                    first_err.get_or_insert(err);
                }
            }
        }
        if !any_ok {
            if let Some(err) = first_err {
                return Err(err);
            }
        }

        let versions: Vec<&str> = found_updates.iter().map(|u| u.version.as_str()).collect();
        let order = install_order(&versions);
        let mut slots: Vec<Option<Update>> = found_updates.into_iter().map(Some).collect();
        let candidates: Vec<Update> = order.into_iter().filter_map(|i| slots[i].take()).collect();

        let info = candidates.first().map(|update| UpdateInfo {
            version: update.version.clone(),
            notes: update.body.clone(),
        });
        *app.state::<PendingUpdate>().0.lock().await = candidates;
        Ok(info)
    }

    pub async fn install(app: &AppHandle) -> AppResult<()> {
        let candidates = std::mem::take(&mut *app.state::<PendingUpdate>().0.lock().await);
        if candidates.is_empty() {
            return Err(AppError::Other("updater: no pending update".into()));
        }

        // The next host is tried only when a download fails, falls silent
        // for `DOWNLOAD_IDLE_TIMEOUT` or its bytes do not verify (`download`
        // checks the signature). Once an installer has been started, its
        // error is final: it may have changed things.
        let mut first_err = None;
        for (attempt, update) in candidates.iter().enumerate() {
            let emitter = app.clone();
            let mut downloaded: u64 = 0;
            // Milliseconds from `started` to the last chunk, read by the
            // watch below; the start itself counts as a chunk, so a host
            // that never answers falls silent like one that stops.
            let started = std::time::Instant::now();
            let last_chunk_ms = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let chunk_mark = std::sync::Arc::clone(&last_chunk_ms);
            let download = update.download(
                move |chunk_len, content_len| {
                    let now = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    chunk_mark.store(now, std::sync::atomic::Ordering::Relaxed);
                    downloaded = downloaded.saturating_add(chunk_len as u64);
                    let _ = emitter.emit(
                        "updater:progress",
                        UpdateProgress {
                            downloaded,
                            total: content_len.unwrap_or(0),
                        },
                    );
                },
                || {},
            );
            tokio::pin!(download);
            // Dropping the download on silence cancels its request.
            let bytes = loop {
                tokio::select! {
                    result = &mut download => break result.map_err(updater_err),
                    _ = tokio::time::sleep(DOWNLOAD_IDLE_POLL) => {
                        let last = std::time::Duration::from_millis(
                            last_chunk_ms.load(std::sync::atomic::Ordering::Relaxed),
                        );
                        if started.elapsed().saturating_sub(last) >= DOWNLOAD_IDLE_TIMEOUT {
                            break Err(AppError::Other(format!(
                                "updater: no data for {} s",
                                DOWNLOAD_IDLE_TIMEOUT.as_secs()
                            )));
                        }
                    }
                }
            };
            match bytes {
                Ok(bytes) => {
                    update.install(bytes).map_err(updater_err)?;
                    // On Windows the NSIS installer launches here and the app
                    // exits via the builder's on_before_exit hook, so this
                    // rarely returns.
                    return Ok(());
                }
                Err(err) => {
                    tracing::warn!(%err, attempt, "update download failed, trying the next host");
                    first_err.get_or_insert(err);
                }
            }
        }
        Err(first_err.unwrap_or_else(|| AppError::Other("updater: no pending update".into())))
    }
}

#[cfg(all(not(debug_assertions), feature = "updater"))]
pub use imp::PendingUpdate;

/// Look for a newer build on the channel in force.
///
/// `automatic` is `true` for the checks nobody asked for — at launch, and
/// after the channel changes. Those answer "nothing new" without a request
/// when the user turned automatic checks off or offline mode is on. A
/// check asked for by hand (`false`) still runs with automatic checks off,
/// and fails with [`OFFLINE_MESSAGE`] in offline mode, or with
/// [`UNAVAILABLE_MESSAGE`] in a build without the updater.
#[tauri::command]
pub async fn check_for_update(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    automatic: bool,
) -> AppResult<Option<UpdateInfo>> {
    let auto_check_on = !automatic || read_auto_check(&state.app_db).await;
    match check_gate(automatic, auto_check_on, crate::offline::is_offline()) {
        CheckGate::Skip => {
            tracing::info!(
                auto_check_on,
                "automatic update check skipped: switched off, or offline mode is on"
            );
            return Ok(None);
        }
        CheckGate::Offline => return Err(AppError::Other(OFFLINE_MESSAGE.into())),
        CheckGate::Read => {}
    }
    let channel = read_channel(&state.app_db).await;
    tracing::info!(
        channel = channel.as_str(),
        automatic,
        "update check allowed: the manifests are read when this build has an updater"
    );
    #[cfg(all(not(debug_assertions), feature = "updater"))]
    {
        imp::check(&app, channel).await
    }
    #[cfg(not(all(not(debug_assertions), feature = "updater")))]
    {
        // No updater plugin in this build (dev / package-manager builds).
        // An automatic check reports "nothing available" so the UI stays
        // quiet; one asked for by hand is told there is nothing to ask,
        // rather than "up to date" for a check that never happened.
        let _ = (&app, channel);
        if automatic {
            Ok(None)
        } else {
            Err(AppError::Other(UNAVAILABLE_MESSAGE.into()))
        }
    }
}

#[tauri::command]
pub async fn install_update(app: tauri::AppHandle) -> AppResult<()> {
    #[cfg(all(not(debug_assertions), feature = "updater"))]
    {
        imp::install(&app).await
    }
    #[cfg(not(all(not(debug_assertions), feature = "updater")))]
    {
        let _ = &app;
        Err(AppError::Other(UNAVAILABLE_MESSAGE.into()))
    }
}

#[cfg(test)]
mod tests;

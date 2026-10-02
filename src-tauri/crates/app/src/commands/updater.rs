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
    #[allow(dead_code)] // used only by the release-gated `imp` module
    fn endpoints(self) -> &'static [&'static str] {
        match self {
            UpdateChannel::Stable => &[STABLE_ENDPOINT],
            UpdateChannel::Beta => &[BETA_ENDPOINT, STABLE_ENDPOINT],
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

async fn read_channel(db: &SqlitePool) -> UpdateChannel {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(KEY_CHANNEL)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    UpdateChannel::parse(raw.as_deref())
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

#[cfg(all(not(debug_assertions), feature = "updater"))]
mod imp {
    use super::*;
    use tauri::{AppHandle, Emitter, Manager};
    use tauri_plugin_updater::{Update, UpdaterExt};
    use tokio::sync::Mutex;

    /// Holds the [`Update`] between [`check_for_update`] and
    /// [`install_update`] — the object owns the verified download URL +
    /// signature and can't cross the IPC boundary, so it lives here.
    #[derive(Default)]
    pub struct PendingUpdate(pub Mutex<Option<Update>>);

    fn updater_err(e: impl std::fmt::Display) -> AppError {
        AppError::Other(format!("updater: {e}"))
    }

    async fn check_endpoint(app: &AppHandle, endpoint: &str) -> AppResult<Option<Update>> {
        let endpoint = tauri::Url::parse(endpoint).map_err(updater_err)?;
        let updater = app
            .updater_builder()
            .endpoints(vec![endpoint])
            .map_err(updater_err)?
            .build()
            .map_err(updater_err)?;
        updater.check().await.map_err(updater_err)
    }

    pub async fn check(app: &AppHandle, channel: UpdateChannel) -> AppResult<Option<UpdateInfo>> {
        // Each manifest only answers with a version newer than this build,
        // so the newest answer is the update. One manifest failing does not
        // hide what another found; only all of them failing is an error.
        let mut newest: Option<Update> = None;
        let mut first_err = None;
        let mut any_ok = false;
        for endpoint in channel.endpoints() {
            match check_endpoint(app, endpoint).await {
                Ok(found) => {
                    any_ok = true;
                    if let Some(update) = found {
                        if newest
                            .as_ref()
                            .map_or(true, |best| is_newer(&update.version, &best.version))
                        {
                            newest = Some(update);
                        }
                    }
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

        let pending = app.state::<PendingUpdate>();
        match newest {
            Some(update) => {
                let info = UpdateInfo {
                    version: update.version.clone(),
                    notes: update.body.clone(),
                };
                *pending.0.lock().await = Some(update);
                Ok(Some(info))
            }
            None => {
                *pending.0.lock().await = None;
                Ok(None)
            }
        }
    }

    pub async fn install(app: &AppHandle) -> AppResult<()> {
        let update = {
            let pending = app.state::<PendingUpdate>();
            let mut slot = pending.0.lock().await;
            slot.take()
        };
        let Some(update) = update else {
            return Err(AppError::Other("updater: no pending update".into()));
        };

        let emitter = app.clone();
        let mut downloaded: u64 = 0;
        update
            .download_and_install(
                move |chunk_len, content_len| {
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
            )
            .await
            .map_err(updater_err)?;
        // On Windows the NSIS installer launches here and the app exits
        // via the builder's on_before_exit hook, so this rarely returns.
        Ok(())
    }
}

#[cfg(all(not(debug_assertions), feature = "updater"))]
pub use imp::PendingUpdate;

#[tauri::command]
pub async fn check_for_update(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> AppResult<Option<UpdateInfo>> {
    let channel = read_channel(&state.app_db).await;
    #[cfg(all(not(debug_assertions), feature = "updater"))]
    {
        imp::check(&app, channel).await
    }
    #[cfg(not(all(not(debug_assertions), feature = "updater")))]
    {
        // No updater plugin in this build (dev / app-store channel) —
        // report "nothing available" so the UI stays quiet.
        let _ = (&app, channel);
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::{is_newer, UpdateChannel, BETA_ENDPOINT, STABLE_ENDPOINT};

    #[test]
    fn a_release_outranks_its_own_betas() {
        assert!(is_newer("1.8.0", "1.8.0-beta.3"));
        assert!(!is_newer("1.8.0-beta.3", "1.8.0"));
        assert!(is_newer("1.8.0-beta.10", "1.8.0-beta.3"));
    }

    #[test]
    fn an_older_release_does_not_displace_a_newer_beta() {
        assert!(!is_newer("1.8.1", "1.9.0-beta.1"));
        assert!(is_newer("1.9.0-beta.1", "1.8.1"));
    }

    #[test]
    fn build_metadata_does_not_make_a_version_newer() {
        assert!(!is_newer("1.8.0+z", "1.8.0+a"));
        assert!(!is_newer("1.8.0+a", "1.8.0+z"));
    }

    #[test]
    fn a_version_that_does_not_parse_never_wins() {
        assert!(!is_newer("not-a-version", "1.8.0"));
        assert!(is_newer("1.8.0", "not-a-version"));
    }

    #[test]
    fn the_beta_channel_reads_the_stable_manifest_too() {
        assert_eq!(UpdateChannel::Stable.endpoints(), [STABLE_ENDPOINT]);
        assert_eq!(
            UpdateChannel::Beta.endpoints(),
            [BETA_ENDPOINT, STABLE_ENDPOINT]
        );
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
        Err(AppError::Other("updater: unavailable in this build".into()))
    }
}

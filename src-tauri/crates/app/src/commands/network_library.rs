//! Adding, checking and removing WebDAV and SMB shares (`crate::netlib`).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::{
    audio::stream_cache,
    error::{AppError, AppResult},
    netlib::{self, fs::NetError, store},
    state::AppState,
    watcher::WatcherManager,
};
use waveflow_core::network_locator;

/// What the form asks for. The password goes one way: nothing ever sends it
/// back to the webview.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSourceInput {
    pub kind: String,
    pub name: String,
    pub endpoint: String,
    #[serde(default)]
    pub share: String,
    #[serde(default)]
    pub root: String,
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(default)]
    pub allow_plain_http: bool,
}

impl std::fmt::Debug for NetworkSourceInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkSourceInput")
            .field("kind", &self.kind)
            .field("endpoint", &self.endpoint)
            .field("share", &self.share)
            .finish_non_exhaustive()
    }
}

/// A source as the interface sees it: no secret, only whether one is set.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSourceView {
    pub id: i64,
    pub uuid: String,
    pub kind: String,
    pub name: String,
    pub endpoint: String,
    pub share: String,
    pub username: Option<String>,
    pub has_password: bool,
    pub plain_http: bool,
    pub state: netlib::SourceState,
    pub last_error: Option<String>,
    pub folders: Vec<NetworkFolderView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkFolderView {
    pub id: i64,
    /// Relative to the share, `/`-separated; empty for its root.
    pub path: String,
}

/// The answer of a connection test, or of an add that failed on it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkTestOutcome {
    pub ok: bool,
    /// `offline`, `unreachable`, `tls`, `auth`, `not-found`, `protocol`,
    /// `plain-http` or `unsupported`, for the interface to word.
    pub code: Option<&'static str>,
    /// The cause as reported: a certificate's fault, a refused connection.
    /// Technical and untranslated, and free of URLs' secrets.
    pub detail: Option<String>,
    /// Directories and files in the chosen folder, to show the path is right.
    pub entries: Option<usize>,
}

impl NetworkTestOutcome {
    fn failed(err: &NetError) -> Self {
        let plain_http = matches!(err, NetError::Protocol(m) if m == "plain-http-not-accepted");
        Self {
            ok: false,
            code: Some(if plain_http { "plain-http" } else { err.code() }),
            detail: (!plain_http).then(|| err.to_string()),
            entries: None,
        }
    }
}

fn row_from_input(input: &NetworkSourceInput) -> Result<store::SourceRow, NetError> {
    let share = input.share.trim().trim_matches(['/', '\\']).to_string();
    let endpoint = match input.kind.as_str() {
        "webdav" => netlib::webdav::parse_endpoint(&input.endpoint, input.allow_plain_http)?
            .as_str()
            .trim_end_matches('/')
            .to_string(),
        other => {
            return Err(NetError::Protocol(format!(
                "unsupported source kind: {other}"
            )))
        }
    };
    let name = match input.name.trim() {
        "" if share.is_empty() => endpoint.clone(),
        "" => share.clone(),
        name => name.to_string(),
    };
    Ok(store::SourceRow {
        id: 0,
        uuid: uuid::Uuid::new_v4().to_string(),
        kind: input.kind.clone(),
        name,
        endpoint,
        share,
        username: input
            .username
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_string),
        secret: input.password.clone().filter(|p| !p.is_empty()),
        allow_plain_http: input.allow_plain_http,
    })
}

fn clean_root(root: &str) -> Result<String, NetError> {
    let root = root.trim().replace('\\', "/");
    let root = root.trim_matches('/').to_string();
    if !network_locator::is_safe_path(&root) {
        return Err(NetError::Protocol("this folder path is not valid".into()));
    }
    Ok(root)
}

/// Try the address, the sign-in and the folder, on the clients' runtime. The
/// backend built here is thrown away: nothing is stored by testing.
async fn try_connect(row: store::SourceRow, root: String) -> Result<usize, NetError> {
    netlib::run(async move {
        if crate::offline::is_offline() {
            return Err(NetError::Offline);
        }
        let fs = netlib::build_fs(&row)?;
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            fs.probe().await?;
            fs.list(&root).await.map(|entries| entries.len())
        })
        .await
        .unwrap_or_else(|_| Err(NetError::Unreachable("no answer in time".into())))
    })
    .await?
}

#[tauri::command]
pub async fn network_source_test(input: NetworkSourceInput) -> AppResult<NetworkTestOutcome> {
    let prepared = row_from_input(&input).and_then(|row| Ok((row, clean_root(&input.root)?)));
    let (row, root) = match prepared {
        Ok(prepared) => prepared,
        Err(err) => return Ok(NetworkTestOutcome::failed(&err)),
    };
    Ok(match try_connect(row, root).await {
        Ok(entries) => NetworkTestOutcome {
            ok: true,
            code: None,
            detail: None,
            entries: Some(entries),
        },
        Err(err) => NetworkTestOutcome::failed(&err),
    })
}

/// The result of adding a share: the folder to scan, or why not.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkAddOutcome {
    pub test: NetworkTestOutcome,
    pub folder_id: Option<i64>,
}

/// Connect, then record the source and its folder. Nothing is stored for an
/// address that does not answer: a typo must not leave a source behind that
/// holds a password. A second folder of a known share joins its source, and
/// the overlap rule of every library folder applies to it.
#[tauri::command]
pub async fn network_source_add(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    library_id: i64,
    input: NetworkSourceInput,
) -> AppResult<NetworkAddOutcome> {
    let prepared = row_from_input(&input).and_then(|row| Ok((row, clean_root(&input.root)?)));
    let (row, root) = match prepared {
        Ok(prepared) => prepared,
        Err(err) => {
            return Ok(NetworkAddOutcome {
                test: NetworkTestOutcome::failed(&err),
                folder_id: None,
            })
        }
    };
    let entries = match try_connect(row.clone(), root.clone()).await {
        Ok(entries) => entries,
        Err(err) => {
            return Ok(NetworkAddOutcome {
                test: NetworkTestOutcome::failed(&err),
                folder_id: None,
            })
        }
    };

    let pool = state.require_profile_pool().await?;
    let existing = store::find_same(&pool, &row.kind, &row.endpoint, &row.share).await?;
    let uuid = existing
        .as_ref()
        .map_or_else(|| row.uuid.clone(), |known| known.uuid.clone());
    let locator = network_locator::build(&uuid, &root)
        .ok_or_else(|| AppError::Other("this folder path is not valid".into()))?;
    let folder_id =
        crate::commands::library::insert_checked_folder(&pool, library_id, &locator).await?;
    match existing {
        Some(known) => {
            store::update_credentials(
                &pool,
                known.id,
                row.username.as_deref(),
                row.secret.as_deref(),
            )
            .await?
        }
        None => {
            if let Err(err) = store::insert(&pool, &row).await {
                // A folder whose source does not exist could neither be scanned
                // nor removed from the card.
                if let Err(undo) = sqlx::query("DELETE FROM library_folder WHERE id = ?")
                    .bind(folder_id)
                    .execute(&*pool)
                    .await
                {
                    tracing::warn!(%undo, "network source: could not undo the folder");
                }
                return Err(err);
            }
        }
    }
    drop(pool);
    netlib::reload_active(&app).await?;
    Ok(NetworkAddOutcome {
        test: NetworkTestOutcome {
            ok: true,
            code: None,
            detail: None,
            entries: Some(entries),
        },
        folder_id: Some(folder_id),
    })
}

#[tauri::command]
pub async fn network_source_list(
    state: tauri::State<'_, AppState>,
) -> AppResult<Vec<NetworkSourceView>> {
    let pool = state.require_profile_pool().await?;
    let mut out = Vec::new();
    for row in store::list(&pool).await? {
        let live = netlib::source(&row.uuid);
        let folders = store::folders_of(&pool, &row.uuid)
            .await?
            .into_iter()
            .map(|(id, path)| NetworkFolderView {
                id,
                path: network_locator::parse(&path)
                    .map(|p| p.path.to_string())
                    .unwrap_or_default(),
            })
            .collect();
        out.push(NetworkSourceView {
            id: row.id,
            uuid: row.uuid,
            kind: row.kind,
            name: row.name,
            endpoint: row.endpoint,
            share: row.share,
            username: row.username,
            has_password: row.secret.is_some(),
            plain_http: row.allow_plain_http,
            state: live
                .as_ref()
                .map_or(netlib::SourceState::Unreachable, |s| s.state()),
            last_error: live.as_ref().and_then(|s| s.last_error()),
            folders,
        });
    }
    Ok(out)
}

/// New credentials for a known source: what an imported profile needs, whose
/// archive holds none, and what a changed password needs.
#[tauri::command]
pub async fn network_source_set_credentials(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    id: i64,
    username: Option<String>,
    password: Option<String>,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    store::update_credentials(
        &pool,
        id,
        username.as_deref().map(str::trim).filter(|u| !u.is_empty()),
        password.as_deref().filter(|p| !p.is_empty()),
    )
    .await?;
    drop(pool);
    netlib::reload_active(&app).await
}

/// Forget a source: its folders, their tracks, what is cached of them, and the
/// stored password.
#[tauri::command]
pub async fn network_source_remove(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    watcher: tauri::State<'_, Arc<WatcherManager>>,
    id: i64,
) -> AppResult<()> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let Some(row) = store::get(&pool, id).await? else {
        return Ok(());
    };
    for (folder_id, _) in store::folders_of(&pool, &row.uuid).await? {
        crate::commands::library::remove_folder_inner(&app, &state, &watcher, &pool, folder_id)
            .await?;
    }
    store::delete(&pool, id).await?;
    let dir = state
        .paths
        .profile_cache_dir(profile_id)
        .join("network-audio");
    netlib::cache::forget_source(&dir, &row.uuid);
    drop(pool);
    netlib::reload_active(&app).await
}

/// The tracks that cannot play right now, for the interface to grey.
#[tauri::command]
pub async fn network_blocked_tracks(state: tauri::State<'_, AppState>) -> AppResult<Vec<i64>> {
    let pool = state.require_profile_pool().await?;
    netlib::blocked_track_ids(&pool).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkCacheInfo {
    pub bytes: u64,
    pub entries: usize,
}

fn cache_dir(state: &AppState, profile_id: i64) -> std::path::PathBuf {
    state
        .paths
        .profile_cache_dir(profile_id)
        .join("network-audio")
}

#[tauri::command]
pub async fn network_cache_info(state: tauri::State<'_, AppState>) -> AppResult<NetworkCacheInfo> {
    let profile_id = state.require_profile_id().await?;
    let (bytes, entries) = stream_cache::info(&cache_dir(&state, profile_id));
    Ok(NetworkCacheInfo { bytes, entries })
}

#[tauri::command]
pub async fn network_cache_clear(state: tauri::State<'_, AppState>) -> AppResult<usize> {
    let profile_id = state.require_profile_id().await?;
    Ok(stream_cache::clear(&cache_dir(&state, profile_id))?)
}

/// The settings card saved a new budget: the writers read it from here, since
/// the decoder thread has no database to ask.
#[tauri::command]
pub async fn network_cache_limit_changed(app: AppHandle) -> AppResult<()> {
    let state = app.state::<AppState>();
    let pool = state.require_profile_pool().await?;
    netlib::reload_cache_limit(&pool).await;
    Ok(())
}

//! The equalizer: its snapshot, its bands and presets, and their persistence.

use std::sync::Arc;

use tauri::{AppHandle, Emitter};

use crate::{
    audio::AudioEngine,
    error::{AppError, AppResult},
    state::AppState,
};

use super::*;

// ─── Equalizer ──────────────────────────────────────────────────────

/// Snapshot returned by `player_get_eq` so the Settings view can
/// hydrate the curve + bypass + preset dropdown without three
/// round-trips. Also broadcast verbatim on `player:eq` after every
/// mutation so the popup + Settings stay in sync when both are open
/// (issue #166 — turning the EQ on from the player-bar popup left the
/// Settings card showing "off" until it was re-mounted).
#[derive(Debug, Clone, serde::Serialize)]
pub struct EqSnapshot {
    pub enabled: bool,
    pub bands_db: Vec<f32>,
    pub band_freqs: Vec<f32>,
    pub max_gain_db: f32,
    pub presets: Vec<EqPresetEntry>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EqPresetEntry {
    pub key: String,
    pub gains: Vec<f32>,
}

/// Build the same payload `player_get_eq` returns so the setters can
/// emit it on `player:eq`. Reading is lock-free (atomic loads) so
/// taking a fresh snapshot per mutation is cheap.
fn build_eq_snapshot(engine: &AudioEngine) -> EqSnapshot {
    let eq = &engine.shared().eq;
    let presets = crate::audio::eq::PRESETS
        .iter()
        .map(|(k, g)| EqPresetEntry {
            key: (*k).to_string(),
            gains: g.to_vec(),
        })
        .collect();
    EqSnapshot {
        enabled: eq.is_enabled(),
        bands_db: eq.read_bands_db().to_vec(),
        band_freqs: crate::audio::eq::BAND_FREQS.to_vec(),
        max_gain_db: crate::audio::eq::MAX_GAIN_DB,
        presets,
    }
}

/// Fire `player:eq` so every mounted EQ surface (Settings card +
/// player-bar popup) refreshes from the same snapshot. Errors are
/// silently dropped — emit only fails when the app is shutting down,
/// at which point a stale UI is irrelevant.
fn emit_eq(app: &AppHandle, engine: &AudioEngine) {
    let _ = app.emit("player:eq", build_eq_snapshot(engine));
}

#[tauri::command]
pub async fn player_get_eq(engine: tauri::State<'_, Arc<AudioEngine>>) -> AppResult<EqSnapshot> {
    Ok(build_eq_snapshot(&engine))
}

#[tauri::command]
pub async fn player_set_eq_enabled(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    let previous = EqState::capture(engine.shared());
    engine.shared().eq.set_enabled(enabled);
    commit_eq(&app, &state, &engine, previous).await
}

#[tauri::command]
pub async fn player_set_eq_band(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    index: usize,
    gain_db: f32,
) -> AppResult<()> {
    let previous = EqState::capture(engine.shared());
    engine.shared().eq.set_band_db(index, gain_db);
    commit_eq(&app, &state, &engine, previous).await
}

#[tauri::command]
pub async fn player_set_eq_preset(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    preset_key: String,
) -> AppResult<()> {
    let Some(gains) = crate::audio::eq::preset_gains(&preset_key) else {
        return Err(AppError::Other(format!("unknown EQ preset: {preset_key}")));
    };
    let previous = EqState::capture(engine.shared());
    engine.shared().eq.set_all_bands_db(&gains);
    commit_eq(&app, &state, &engine, previous).await
}

/// The EQ as it was before a change, to put back if the change cannot
/// be saved.
struct EqState {
    enabled: bool,
    bands: Vec<f32>,
}

impl EqState {
    fn capture(shared: &crate::audio::state::SharedPlayback) -> Self {
        Self {
            enabled: shared.eq.is_enabled(),
            bands: shared.eq.read_bands_db().to_vec(),
        }
    }
}

/// Save an EQ change the engine has just applied, then tell every EQ
/// surface. The engine goes first here, unlike the other settings, because
/// it is what turns a preset into its six gains; so a change the database
/// refuses is undone in the engine instead, and the `player:eq` that
/// follows brings the controls back to it along with the error.
async fn commit_eq(
    app: &AppHandle,
    state: &AppState,
    engine: &AudioEngine,
    previous: EqState,
) -> AppResult<()> {
    let saved = persist_eq(state, engine.shared()).await;
    if saved.is_err() {
        let eq = &engine.shared().eq;
        eq.set_all_bands_db(&previous.bands);
        eq.set_enabled(previous.enabled);
    }
    emit_eq(app, engine);
    saved
}

/// Persist the EQ snapshot under two keys: `audio.eq_enabled` (bool)
/// and `audio.eq_bands` (JSON array of f32). Splitting the bypass
/// from the bands lets the user toggle the EQ off without losing
/// their custom curve.
async fn persist_eq(
    state: &AppState,
    shared: &crate::audio::state::SharedPlayback,
) -> AppResult<()> {
    let Ok(pool) = state.require_profile_pool().await else {
        return Ok(());
    };
    let enabled = shared.eq.is_enabled();
    let bands = shared.eq.read_bands_db();
    let bands_json = serde_json::to_string(&bands.to_vec()).unwrap_or_else(|_| "[]".into());
    persist_setting(
        &pool,
        "audio.eq_enabled",
        if enabled { "true" } else { "false" },
        SettingType::Bool,
    )
    .await?;
    persist_setting(&pool, "audio.eq_bands", &bands_json, SettingType::Json).await
}

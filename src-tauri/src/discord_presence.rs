//! Discord Rich Presence integration.
//!
//! Mirrors the architecture of [`crate::media_controls`]: a dedicated
//! thread owns the `DiscordIpcClient` (which is `!Send` between
//! threads on Windows because it wraps a Win32 named pipe handle), and
//! a `crossbeam-channel` carries update messages from the player code.
//!
//! Update flow:
//! - `commands::player::emit_track_changed` resolves the Deezer cover
//!   URL (best-effort, async DB lookup) and pushes new metadata.
//! - `audio::decoder::transition_state` pushes Playing / Paused
//!   transitions so the elapsed/remaining timestamps refresh.
//!
//! Opt-in: a missing `app_setting['integrations.discord_rpc']` row
//! means "off" — the thread sits idle, never connecting to Discord.
//! The Settings toggle flips the flag and tells this module to either
//! connect-and-publish or disconnect-and-clear.

use std::time::{SystemTime, UNIX_EPOCH};

use crossbeam_channel::{unbounded, Sender};
use discord_rich_presence::{
    activity::{Activity, ActivityType, Assets, Timestamps},
    DiscordIpc, DiscordIpcClient,
};

use crate::audio::PlayerState;

/// Discord Application ID for WaveFlow. Hard-coded because it's a
/// public identifier, not a secret — Discord uses it to look up the
/// app's display name and asset library when rendering the presence
/// card. Changing it requires a new Discord application + reuploaded
/// assets, so it lives in source.
const DISCORD_CLIENT_ID: &str = "1502611865698570291";

/// Asset key (uploaded under "Art Assets" in the Discord developer
/// portal) used as `large_image` whenever no Deezer cover URL is
/// available for the current track.
const FALLBACK_LARGE_IMAGE: &str = "waveflow_logo";

/// Cached metadata held inside the presence thread so we can re-emit
/// after a state transition (Playing ↔ Paused) without making the
/// caller re-pass the title/artist/album triple.
#[derive(Default, Clone)]
struct CachedMetadata {
    title: String,
    artist: Option<String>,
    album: Option<String>,
    cover_url: Option<String>,
    duration_ms: i64,
    /// Wall-clock time (unix seconds) at which this metadata was
    /// pushed plus the position within the track at that moment. Used
    /// to compute the `end` timestamp Discord uses to render the
    /// `01:01 ──── 03:17` progress bar.
    started_at_unix: i64,
    started_position_ms: u64,
}

/// Update message sent to the controls thread.
enum Msg {
    /// Enable / disable presence at runtime. Disabling clears the
    /// activity and disconnects the IPC client; re-enabling
    /// reconnects on the next `Metadata` or `Playback` push.
    SetEnabled(bool),
    Metadata(CachedMetadata),
    Playback {
        state: PlayerState,
        position_ms: u64,
    },
    /// Clear the activity (used when playback stops entirely or when
    /// the user toggles RPC off). Currently reachable only through
    /// `DiscordPresenceHandle::clear`, which has no caller yet — kept
    /// in the API surface so a future "stop button clears presence"
    /// behaviour doesn't need to revisit the protocol.
    #[allow(dead_code)]
    Clear,
}

/// Handle exposed via `tauri::State`. Cheap to clone — every method
/// just sends on the inner channel and returns immediately.
pub struct DiscordPresenceHandle {
    tx: Sender<Msg>,
}

impl DiscordPresenceHandle {
    pub fn set_enabled(&self, enabled: bool) {
        let _ = self.tx.send(Msg::SetEnabled(enabled));
    }

    pub fn update_metadata(
        &self,
        title: String,
        artist: Option<String>,
        album: Option<String>,
        cover_url: Option<String>,
        duration_ms: i64,
        position_ms: u64,
    ) {
        let started_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let _ = self.tx.send(Msg::Metadata(CachedMetadata {
            title,
            artist,
            album,
            cover_url,
            duration_ms,
            started_at_unix,
            started_position_ms: position_ms,
        }));
    }

    pub fn update_playback(&self, state: PlayerState, position_ms: u64) {
        let _ = self.tx.send(Msg::Playback { state, position_ms });
    }

    #[allow(dead_code)]
    pub fn clear(&self) {
        let _ = self.tx.send(Msg::Clear);
    }
}

/// Spawn the presence thread. Returns a handle even when Discord
/// isn't running — the IPC connection is lazy and only attempted
/// after the user enables RPC. Returns `None` only if the OS thread
/// itself can't be spawned, which is effectively never.
pub fn init(initial_enabled: bool) -> Option<DiscordPresenceHandle> {
    let (tx, rx) = unbounded::<Msg>();

    let spawn = std::thread::Builder::new()
        .name("waveflow-discord-rpc".into())
        .spawn(move || {
            let mut enabled = initial_enabled;
            let mut client: Option<DiscordIpcClient> = None;
            let mut cached: Option<CachedMetadata> = None;
            let mut last_state = PlayerState::Idle;

            while let Ok(msg) = rx.recv() {
                match msg {
                    Msg::SetEnabled(value) => {
                        enabled = value;
                        if !enabled {
                            disconnect(&mut client);
                        } else if let Some(meta) = cached.clone() {
                            // Re-publish whatever we last knew so the
                            // user sees presence immediately after
                            // flipping the toggle on (instead of
                            // having to skip a track).
                            ensure_connected(&mut client);
                            push_activity(&mut client, &meta, last_state);
                        }
                    }
                    Msg::Metadata(meta) => {
                        cached = Some(meta.clone());
                        if enabled {
                            ensure_connected(&mut client);
                            push_activity(&mut client, &meta, last_state);
                        }
                    }
                    Msg::Playback { state, position_ms } => {
                        last_state = state;
                        if matches!(state, PlayerState::Idle | PlayerState::Ended) {
                            if enabled {
                                clear_activity(&mut client);
                            }
                            continue;
                        }
                        // Loading is a brief transient — leave the
                        // last activity in place so the card doesn't
                        // flicker between tracks.
                        if matches!(state, PlayerState::Loading) {
                            continue;
                        }
                        if let Some(meta) = cached.as_mut() {
                            // Re-anchor the timestamps so the
                            // progress bar restarts from the new
                            // position after a seek or a
                            // pause→resume.
                            meta.started_at_unix = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_secs() as i64)
                                .unwrap_or(0);
                            meta.started_position_ms = position_ms;
                            if enabled {
                                ensure_connected(&mut client);
                                push_activity(&mut client, meta, state);
                            }
                        }
                    }
                    Msg::Clear => {
                        cached = None;
                        if enabled {
                            clear_activity(&mut client);
                        }
                    }
                }
            }
        });

    match spawn {
        Ok(_) => Some(DiscordPresenceHandle { tx }),
        Err(err) => {
            tracing::warn!(%err, "discord_presence: failed to spawn thread");
            None
        }
    }
}

/// Connect to the Discord IPC if we don't already have a live
/// client. A connection failure leaves `client = None` so the next
/// push retries — Discord may have just been opened.
fn ensure_connected(client: &mut Option<DiscordIpcClient>) {
    if client.is_some() {
        return;
    }
    let mut new_client = DiscordIpcClient::new(DISCORD_CLIENT_ID);
    if let Err(err) = new_client.connect() {
        tracing::warn!(%err, "discord_presence: IPC connect failed (Discord not running?)");
        return;
    }
    tracing::info!("discord_presence: connected");
    *client = Some(new_client);
}

fn disconnect(client: &mut Option<DiscordIpcClient>) {
    if let Some(mut c) = client.take() {
        let _ = c.clear_activity();
        let _ = c.close();
        tracing::info!("discord_presence: disconnected");
    }
}

fn clear_activity(client: &mut Option<DiscordIpcClient>) {
    if let Some(c) = client.as_mut() {
        if let Err(err) = c.clear_activity() {
            tracing::warn!(%err, "discord_presence: clear_activity failed");
            // Drop the client so the next push reconnects.
            *client = None;
        }
    }
}

/// Build and push an Activity for the cached track. Drops the IPC
/// client on failure so the next push attempts a fresh connection
/// (Discord can disappear at any time).
fn push_activity(client: &mut Option<DiscordIpcClient>, meta: &CachedMetadata, state: PlayerState) {
    let Some(c) = client.as_mut() else {
        return;
    };

    // Spotify-style layout: title (details) / artist (state) / album
    // shown inline below by Discord as `large_text`. Discord requires
    // `state` to be ≥ 2 characters, so we fall back to the album when
    // there's no artist tag (better than dropping the line entirely).
    let state_line = match (meta.artist.as_deref(), meta.album.as_deref()) {
        (Some(a), _) if !a.is_empty() => a.to_string(),
        (_, Some(b)) if !b.is_empty() => b.to_string(),
        _ => String::from("WaveFlow"),
    };

    let large_image = meta
        .cover_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(FALLBACK_LARGE_IMAGE);
    let large_text = meta
        .album
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("WaveFlow");

    // `name` is required by Discord for the activity to actually
    // render — without it the IPC accepts the payload silently but
    // nothing shows on the user's profile. `activity_type` =
    // Listening switches the header from "Playing WaveFlow" to
    // "Listening to WaveFlow" (Spotify-style).
    let mut activity = Activity::new()
        .name("WaveFlow")
        .activity_type(ActivityType::Listening)
        .details(meta.title.clone())
        .state(state_line)
        .assets(
            Assets::new()
                .large_image(large_image.to_string())
                .large_text(large_text.to_string())
                .small_image(match state {
                    PlayerState::Paused => "pause",
                    _ => "play",
                })
                .small_text(match state {
                    PlayerState::Paused => "En pause",
                    _ => "En lecture",
                }),
        );

    // Only attach timestamps for active playback. While paused, the
    // progress bar would keep ticking on Discord's side because it
    // computes elapsed time from the wall clock — confusing UX.
    if matches!(state, PlayerState::Playing) && meta.duration_ms > 0 {
        let remaining_ms = meta
            .duration_ms
            .saturating_sub(meta.started_position_ms as i64)
            .max(0) as u64;
        let end_unix = meta.started_at_unix + (remaining_ms as i64) / 1000;
        let start_unix = meta
            .started_at_unix
            .saturating_sub(meta.started_position_ms as i64 / 1000);
        activity = activity.timestamps(Timestamps::new().start(start_unix).end(end_unix));
    }

    if let Err(err) = c.set_activity(activity) {
        tracing::warn!(%err, "discord_presence: set_activity failed");
        // Force a reconnect on the next push.
        let _ = c.close();
        *client = None;
    }
}

/// Read the persisted opt-in flag from `app_setting`. Defaults to
/// `true` (on) when the row is missing — matches the Spotify-style
/// expectation that Rich Presence "just works" out of the box. Users
/// can opt out via the Settings toggle, which writes the literal
/// "false" and switches this branch.
pub async fn read_enabled(app_db: &sqlx::SqlitePool) -> bool {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM app_setting WHERE key = 'integrations.discord_rpc'")
            .fetch_optional(app_db)
            .await
            .ok()
            .flatten();
    match raw.as_deref() {
        Some("false") => false,
        _ => true,
    }
}

/// Best-effort lookup of a public Deezer cover URL for the given
/// track. Two-stage:
///
/// 1. Cache hit — JOIN `track → album → deezer_album` in the per-
///    profile pool. Cheap, no network.
/// 2. Cache miss but the track has an `album_id` — call
///    [`crate::commands::deezer::enrich_album_inner`] to search
///    Deezer by title+artist. The result is persisted in
///    `deezer_album` for future lookups, so this auto-enrichment
///    only fires once per untouched album.
///
/// Returns `None` if the track has no album row at all, or if the
/// Deezer search came back empty. Discord requires a publicly
/// reachable HTTPS URL for `large_image`, which Deezer's CDN serves
/// natively — local artwork files are intentionally not used as a
/// fallback because Discord propagates the URL to other users'
/// clients (who can't reach our local files or `127.0.0.1` shim).
pub async fn resolve_cover_url(
    pool: &sqlx::SqlitePool,
    artwork_dir: &std::path::Path,
    track_id: i64,
) -> Option<String> {
    // Stage 1: cached URL via existing deezer_id link.
    let cached: Option<String> = sqlx::query_scalar(
        "SELECT da.cover_url
           FROM track t
           JOIN album a ON a.id = t.album_id
           JOIN deezer_album da ON da.deezer_id = a.deezer_id
          WHERE t.id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    if let Some(url) = cached.filter(|u| !u.is_empty()) {
        return Some(url);
    }

    // Stage 2: trigger an on-demand enrichment for this album. The
    // helper handles cache check + Deezer search + DB upsert, so a
    // subsequent call will hit the cache. Returns `None` if the
    // album doesn't exist or Deezer has nothing matching.
    let album_id: Option<i64> = sqlx::query_scalar("SELECT album_id FROM track WHERE id = ?")
        .bind(track_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    let album_id = album_id?;
    match crate::commands::deezer::enrich_album_inner(pool, artwork_dir, album_id).await {
        Ok(enrichment) => enrichment.cover_url.filter(|u| !u.is_empty()),
        Err(err) => {
            tracing::debug!(album_id, %err, "discord_presence: auto-enrich failed");
            None
        }
    }
}

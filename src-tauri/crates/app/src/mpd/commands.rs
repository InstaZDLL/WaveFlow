//! Dispatch: turn a parsed [`Command`] into a [`Response`] or an [`Ack`].
//!
//! Everything here runs inside a per-connection task on the MPD
//! worker's runtime. State is read straight out of [`SharedPlayback`]
//! (atomics) and the profile pool; mutations go through
//! [`crate::player_actions`] so the tray, the media keys and MPD all
//! move the player the same way and all emit the same events.

use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

use tauri::{AppHandle, Manager};

use super::{
    config::MpdConfig,
    idle::{IdleBus, Subsystem},
    protocol::{Ack, Command, Range, Response, SingleMode, ACK_ERROR_ARG, ACK_ERROR_NO_EXIST},
    songs,
};
use crate::{
    audio::{engine::AudioCmd, state::PlayerState, AudioEngine},
    player_actions,
    queue::{self, RepeatMode},
    state::AppState,
};

/// Log/trace label so a failure in `player_actions` can be traced back
/// to this surface.
const SURFACE: &str = "mpd";

/// Commands we advertise in response to `commands`. Clients hide UI for
/// anything missing here, so it must stay in step with [`dispatch`] —
/// listing something unimplemented produces a button that ACKs.
pub const SUPPORTED_COMMANDS: &[&str] = &[
    "clear",
    "close",
    "commands",
    "consume",
    "currentsong",
    "decoders",
    "delete",
    "deleteid",
    "getvol",
    "idle",
    "move",
    "moveid",
    "next",
    "noidle",
    "notcommands",
    "outputs",
    "password",
    "pause",
    "ping",
    "play",
    "playid",
    "playlistid",
    "playlistinfo",
    "previous",
    "random",
    "repeat",
    "seek",
    "seekcur",
    "seekid",
    "setvol",
    "shuffle",
    "single",
    "stats",
    "status",
    "stop",
    "tagtypes",
    "urlhandlers",
    "volume",
];

/// Tags we populate on a song. `tagtypes` drives which columns a client
/// offers to display, so listing one we never send leaves blank columns.
const TAG_TYPES: &[&str] = &[
    "Artist",
    "Album",
    "AlbumArtist",
    "Title",
    "Track",
    "Disc",
    "Date",
];

/// Shared per-server context handed to every connection task.
#[derive(Clone)]
pub struct Ctx {
    pub app: AppHandle,
    pub config: MpdConfig,
    pub idle: IdleBus,
    /// MPD's `playlist` field in `status` — a version counter clients
    /// compare against to notice the queue changed. Monotonic; the
    /// absolute value is meaningless, only the change is.
    pub playlist_version: Arc<AtomicU32>,
}

impl Ctx {
    fn engine(&self) -> tauri::State<'_, Arc<AudioEngine>> {
        self.app.state::<Arc<AudioEngine>>()
    }

    /// Bump the queue version and wake anyone idling on `playlist`.
    fn queue_changed(&self) {
        self.playlist_version.fetch_add(1, Ordering::Relaxed);
        self.idle.notify(Subsystem::Playlist);
    }
}

/// Per-connection authentication state.
pub struct Session {
    pub authenticated: bool,
}

impl Session {
    pub fn new(config: &MpdConfig) -> Self {
        Self {
            authenticated: !config.requires_auth(),
        }
    }
}

fn ack_arg(command: &str, message: &str) -> Ack {
    Ack::new(ACK_ERROR_ARG, command, message)
}

/// Persist the effective volume (0–100) into `profile_setting['player.volume']`
/// so an MPD-driven volume change survives a restart, exactly as
/// `commands::player::player_set_volume` does. Best-effort — a missing profile
/// pool is not worth failing the command over; the engine was already told.
async fn persist_volume(ctx: &Ctx, volume_0_100: i64) {
    let state = ctx.app.state::<AppState>();
    if let Ok(pool) = state.require_profile_pool().await {
        let now = chrono::Utc::now().timestamp_millis();
        let _ = sqlx::query(
            "UPDATE profile_setting SET value = ?, updated_at = ? WHERE key = 'player.volume'",
        )
        .bind(volume_0_100.to_string())
        .bind(now)
        .execute(&*pool)
        .await;
    }
}

/// MPD sends seek targets as fractional seconds; the engine wants
/// milliseconds.
///
/// Scaling **before** the cast matters: `seconds as u64 * 1000` floors
/// to a whole second first and silently drops the fraction, so
/// `seek 0 12.5` would land at 12.0 s. Extracted because the same
/// conversion is needed by `seek`, `seekid` and `seekcur`, and one of
/// the three had it wrong.
fn seconds_to_ms(seconds: f64) -> u64 {
    (seconds.max(0.0) * 1000.0) as u64
}

/// Is `position` the queue slot of the track the engine already has loaded
/// (playing or paused)? `seek` uses this to scrub in place rather than
/// reloading — and restarting from 0 — the very track it's seeking within.
async fn is_current_track(ctx: &Ctx, pool: &sqlx::SqlitePool, position: i64) -> bool {
    let loaded = matches!(
        ctx.engine().shared().state(),
        PlayerState::Playing | PlayerState::Paused
    );
    loaded && position == queue::current_index(pool).await
}

/// Map WaveFlow's tri-state repeat onto MPD's two independent flags.
///
/// MPD spells "repeat the current track" as `repeat 1` + `single 1`,
/// whereas WaveFlow has a single enum. The mapping is lossless in this
/// direction; see [`repeat_from_mpd`] / [`single_from_mpd`] for the
/// inverse, which has to preserve the other flag to stay round-trippable.
fn repeat_flags(mode: RepeatMode) -> (bool, bool) {
    match mode {
        RepeatMode::Off => (false, false),
        RepeatMode::All => (true, false),
        RepeatMode::One => (true, true),
    }
}

fn repeat_from_mpd(current: RepeatMode, repeat: bool) -> RepeatMode {
    match (repeat, current) {
        // Turning repeat off clears single too — MPD's `single` without
        // `repeat` means "stop after this track", which WaveFlow has no
        // equivalent for, so Off is the honest landing spot.
        (false, _) => RepeatMode::Off,
        // Keep One if we were already there: the client only touched
        // the repeat bit.
        (true, RepeatMode::One) => RepeatMode::One,
        (true, _) => RepeatMode::All,
    }
}

fn single_from_mpd(current: RepeatMode, single: bool) -> RepeatMode {
    match (single, current) {
        (true, _) => RepeatMode::One,
        (false, RepeatMode::One) => RepeatMode::All,
        (false, other) => other,
    }
}

/// Build the `status` response.
async fn status(ctx: &Ctx) -> Result<Response, Ack> {
    let state = ctx.app.state::<AppState>();
    let engine = ctx.engine();
    let shared = engine.shared();

    let mut out = Response::new();
    out.push("volume", (shared.volume() * 100.0).round() as i64);

    let pool = state
        .require_profile_pool()
        .await
        .map_err(|_| Ack::new(ACK_ERROR_NO_EXIST, "status", "no active profile"))?;

    let repeat_mode = queue::read_repeat_mode(&pool).await;
    let (repeat, single) = repeat_flags(repeat_mode);
    let random = queue::read_shuffle(&pool).await;
    out.push("repeat", u8::from(repeat));
    out.push("random", u8::from(random));
    out.push("single", u8::from(single));
    // WaveFlow has no consume mode; reporting 1 would make a client
    // expect entries to vanish as they play.
    out.push("consume", 0);
    out.push("playlist", ctx.playlist_version.load(Ordering::Relaxed));

    let len = queue::queue_length(&pool).await.unwrap_or(0);
    out.push("playlistlength", len);

    let player_state = shared.state();
    out.push(
        "state",
        match player_state {
            PlayerState::Playing => "play",
            PlayerState::Paused => "pause",
            _ => "stop",
        },
    );

    // A Web Radio session owns the engine under a negative sentinel id
    // with no `track` row and no queue entry, so it has no `song` /
    // `songid` to report. `player_get_state` takes the same branch —
    // without it we'd surface the last *library* track and a client
    // would show a song that isn't playing.
    let is_radio = shared.current_track_id.load(Ordering::Relaxed) < 0;

    // Resolved once: `status` is the command clients poll hardest, so
    // it should not cost two cursor reads and two row lookups.
    let current = if !is_radio && len > 0 {
        let index = queue::current_index(&pool).await;
        out.push("song", index);
        songs::by_position(&pool, index).await.ok().flatten()
    } else {
        None
    };
    if let Some(song) = &current {
        out.push("songid", song.queue_id);
    }

    if matches!(player_state, PlayerState::Playing | PlayerState::Paused) {
        let elapsed_ms = shared.current_position_ms();
        let elapsed = elapsed_ms as f64 / 1000.0;
        out.push("elapsed", format!("{elapsed:.3}"));

        // A live stream has no total duration; sending `time: 12:0`
        // makes clients draw a zero-length seek bar.
        if let Some(song) = &current {
            let total = song.duration_ms.max(0) / 1000;
            out.push(
                "duration",
                format!("{:.3}", song.duration_ms.max(0) as f64 / 1000.0),
            );
            out.push("time", format!("{}:{}", elapsed_ms / 1000, total));
        }

        let rate = shared.sample_rate.load(Ordering::Relaxed);
        let channels = shared.channels.load(Ordering::Relaxed);
        if rate > 0 {
            // MPD's `audio` field is `rate:bits:channels`. We feed the
            // device f32 samples, and MPD spells float as `dsd`-style
            // `f` in the bits slot.
            out.push("audio", format!("{rate}:f:{channels}"));
        }
    }

    Ok(out)
}

/// `currentsong` — the queue entry under the cursor, or nothing.
async fn current_song(ctx: &Ctx) -> Result<Response, Ack> {
    let state = ctx.app.state::<AppState>();
    let engine = ctx.engine();
    let mut out = Response::new();

    // Same radio branch as `status`: no queue entry to describe.
    if engine.shared().current_track_id.load(Ordering::Relaxed) < 0 {
        return Ok(out);
    }

    let Ok(pool) = state.require_profile_pool().await else {
        return Ok(out);
    };
    let index = queue::current_index(&pool).await;
    if let Ok(Some(song)) = songs::by_position(&pool, index).await {
        song.write_into(&mut out);
    }
    Ok(out)
}

/// Dispatch one command.
///
/// Returns `Err(Ack)` for anything the client got wrong; the connection
/// layer stamps the command-list index onto it.
pub async fn dispatch(ctx: &Ctx, session: &mut Session, cmd: Command) -> Result<Response, Ack> {
    // Authentication gate. `password`, `ping` and `close` stay reachable
    // so a client can actually authenticate and so a health probe works.
    if !session.authenticated
        && !matches!(cmd, Command::Password(_) | Command::Ping | Command::Close)
    {
        return Err(Ack::new(
            super::protocol::ACK_ERROR_PERMISSION,
            "",
            "you don't have permission for this command",
        ));
    }

    match cmd {
        Command::Ping | Command::Close => Ok(Response::new()),

        Command::Password(supplied) => {
            // Constant-time-ish comparison is overkill for a LAN control
            // protocol that transmits the password in cleartext anyway;
            // the honest mitigation is documented as "use this on a
            // network you trust".
            if !ctx.config.requires_auth() || supplied == ctx.config.password {
                session.authenticated = true;
                Ok(Response::new())
            } else {
                Err(Ack::new(
                    super::protocol::ACK_ERROR_PASSWORD,
                    "password",
                    "incorrect password",
                ))
            }
        }

        Command::Commands => {
            let mut out = Response::new();
            for c in SUPPORTED_COMMANDS {
                out.push("command", c);
            }
            Ok(out)
        }
        Command::NotCommands => Ok(Response::new()),
        Command::TagTypes => {
            let mut out = Response::new();
            for t in TAG_TYPES {
                out.push("tagtype", t);
            }
            Ok(out)
        }
        // We play local files only — no `http://` handler to advertise.
        Command::UrlHandlers | Command::Decoders => Ok(Response::new()),
        Command::Outputs => {
            let mut out = Response::new();
            out.push("outputid", 0);
            out.push(
                "outputname",
                ctx.engine()
                    .current_output_device()
                    .unwrap_or_else(|| "default".into()),
            );
            out.push("outputenabled", 1);
            Ok(out)
        }

        Command::Status => status(ctx).await,
        Command::CurrentSong => current_song(ctx).await,
        Command::Stats => {
            let state = ctx.app.state::<AppState>();
            let mut out = Response::new();
            if let Ok(pool) = state.require_profile_pool().await {
                if let Ok(s) = songs::stats(&pool).await {
                    out.push("artists", s.artists);
                    out.push("albums", s.albums);
                    out.push("songs", s.songs);
                    out.push("db_playtime", s.db_playtime);
                }
            }
            out.push("uptime", 0);
            out.push("playtime", 0);
            Ok(out)
        }

        Command::PlaylistInfo(range) => {
            let state = ctx.app.state::<AppState>();
            let mut out = Response::new();
            let Ok(pool) = state.require_profile_pool().await else {
                return Ok(out);
            };
            let list = match range {
                None => songs::list(&pool).await,
                Some(r) => {
                    let len = queue::queue_length(&pool).await.unwrap_or(0) as u32;
                    match r.resolve(len) {
                        Some((start, end)) => songs::list_range(&pool, start, end).await,
                        // An out-of-bounds range is an argument error in
                        // MPD, not an empty result.
                        None => return Err(ack_arg("playlistinfo", "Bad song index")),
                    }
                }
            };
            for song in list.map_err(|_| ack_arg("playlistinfo", "queue read failed"))? {
                song.write_into(&mut out);
            }
            Ok(out)
        }

        Command::PlaylistId(id) => {
            let state = ctx.app.state::<AppState>();
            let mut out = Response::new();
            let Ok(pool) = state.require_profile_pool().await else {
                return Ok(out);
            };
            match id {
                None => {
                    for song in songs::list(&pool).await.unwrap_or_default() {
                        song.write_into(&mut out);
                    }
                }
                Some(id) => match songs::by_queue_id(&pool, id).await {
                    Ok(Some(song)) => song.write_into(&mut out),
                    _ => return Err(Ack::new(ACK_ERROR_NO_EXIST, "playlistid", "No such song")),
                },
            }
            Ok(out)
        }

        Command::Play(pos) => {
            match pos {
                None => {
                    let _ = ctx.engine().send(AudioCmd::Resume);
                }
                Some(p) => {
                    // A position past the end is an argument error in MPD, not
                    // a silent no-op / clamp. One snapshot for the bounds check
                    // AND the jump, so a profile switch can't slip a different
                    // queue in between the two.
                    let state = ctx.app.state::<AppState>();
                    let (pool, profile_id) = state
                        .require_profile_snapshot()
                        .await
                        .map_err(|_| Ack::new(ACK_ERROR_NO_EXIST, "play", "no active profile"))?;
                    let len = queue::queue_length(&pool).await.unwrap_or(0) as u32;
                    if p >= len {
                        return Err(ack_arg("play", "Bad song index"));
                    }
                    player_actions::play_at_index_with(&ctx.app, &pool, profile_id, p as i64, SURFACE)
                        .await;
                }
            }
            ctx.idle.notify(Subsystem::Player);
            Ok(Response::new())
        }

        Command::PlayId(id) => {
            match id {
                None => {
                    let _ = ctx.engine().send(AudioCmd::Resume);
                }
                Some(id) => {
                    // One snapshot for the id→position lookup AND the jump, so
                    // the position can't be resolved against one profile then
                    // played against another.
                    let state = ctx.app.state::<AppState>();
                    let (pool, profile_id) = state
                        .require_profile_snapshot()
                        .await
                        .map_err(|_| Ack::new(ACK_ERROR_NO_EXIST, "playid", "no active profile"))?;
                    match queue::position_of_queue_id(&pool, id as i64).await {
                        Ok(Some(position)) => {
                            player_actions::play_at_index_with(
                                &ctx.app, &pool, profile_id, position, SURFACE,
                            )
                            .await;
                        }
                        _ => return Err(Ack::new(ACK_ERROR_NO_EXIST, "playid", "No such song")),
                    }
                }
            }
            ctx.idle.notify(Subsystem::Player);
            Ok(Response::new())
        }

        Command::Pause(want) => {
            let engine = ctx.engine();
            let cmd = match want {
                Some(true) => AudioCmd::Pause,
                Some(false) => AudioCmd::Resume,
                // Bare `pause` toggles, which is what a remote's single
                // play/pause button sends.
                None => match engine.shared().state() {
                    PlayerState::Playing => AudioCmd::Pause,
                    _ => AudioCmd::Resume,
                },
            };
            let _ = engine.send(cmd);
            ctx.idle.notify(Subsystem::Player);
            Ok(Response::new())
        }

        Command::Stop => {
            let _ = ctx.engine().send(AudioCmd::Stop);
            ctx.idle.notify(Subsystem::Player);
            Ok(Response::new())
        }

        Command::Next => {
            player_actions::next(&ctx.app, SURFACE).await;
            ctx.idle.notify(Subsystem::Player);
            ctx.queue_changed();
            Ok(Response::new())
        }

        Command::Previous => {
            player_actions::previous(&ctx.app, SURFACE).await;
            ctx.idle.notify(Subsystem::Player);
            ctx.queue_changed();
            Ok(Response::new())
        }

        Command::Seek(pos, seconds) => {
            let state = ctx.app.state::<AppState>();
            let (pool, profile_id) = state
                .require_profile_snapshot()
                .await
                .map_err(|_| Ack::new(ACK_ERROR_NO_EXIST, "seek", "no active profile"))?;
            let len = queue::queue_length(&pool).await.unwrap_or(0) as u32;
            if pos >= len {
                return Err(ack_arg("seek", "Bad song index"));
            }
            // Seeking within the track that's already loaded must NOT reload
            // it (which restarts from 0) — only switch tracks when the target
            // differs from the current cursor. Scrubbing the progress bar is
            // exactly this case.
            if !is_current_track(ctx, &pool, pos as i64).await {
                player_actions::play_at_index_with(&ctx.app, &pool, profile_id, pos as i64, SURFACE)
                    .await;
            }
            let _ = ctx.engine().send(AudioCmd::Seek(seconds_to_ms(seconds)));
            ctx.idle.notify(Subsystem::Player);
            Ok(Response::new())
        }

        Command::SeekId(id, seconds) => {
            let state = ctx.app.state::<AppState>();
            let (pool, profile_id) = state
                .require_profile_snapshot()
                .await
                .map_err(|_| Ack::new(ACK_ERROR_NO_EXIST, "seekid", "no active profile"))?;
            match queue::position_of_queue_id(&pool, id as i64).await {
                Ok(Some(position)) => {
                    if !is_current_track(ctx, &pool, position).await {
                        player_actions::play_at_index_with(
                            &ctx.app, &pool, profile_id, position, SURFACE,
                        )
                        .await;
                    }
                    let _ = ctx.engine().send(AudioCmd::Seek(seconds_to_ms(seconds)));
                    ctx.idle.notify(Subsystem::Player);
                    Ok(Response::new())
                }
                _ => Err(Ack::new(ACK_ERROR_NO_EXIST, "seekid", "No such song")),
            }
        }

        Command::SeekCur { seconds, relative } => {
            let engine = ctx.engine();
            let target_ms = if relative {
                let current = engine.shared().current_position_ms() as f64;
                // `seconds` is signed here, so this is the one place the
                // clamp has to happen after the addition.
                (current + seconds * 1000.0).max(0.0) as u64
            } else {
                seconds_to_ms(seconds)
            };
            let _ = engine.send(AudioCmd::Seek(target_ms));
            ctx.idle.notify(Subsystem::Player);
            Ok(Response::new())
        }

        Command::SetVol(v) => {
            let _ = ctx.engine().send(AudioCmd::SetVolume(v as f32 / 100.0));
            persist_volume(ctx, v as i64).await;
            ctx.idle.notify(Subsystem::Mixer);
            Ok(Response::new())
        }

        Command::GetVol => {
            let mut out = Response::new();
            out.push(
                "volume",
                (ctx.engine().shared().volume() * 100.0).round() as i64,
            );
            Ok(out)
        }

        Command::VolumeDelta(delta) => {
            let engine = ctx.engine();
            let current = (engine.shared().volume() * 100.0).round() as i32;
            let next = (current + delta).clamp(0, 100);
            let _ = engine.send(AudioCmd::SetVolume(next as f32 / 100.0));
            persist_volume(ctx, next as i64).await;
            ctx.idle.notify(Subsystem::Mixer);
            Ok(Response::new())
        }

        Command::Clear => {
            let state = ctx.app.state::<AppState>();
            let pool = state
                .require_profile_pool()
                .await
                .map_err(|_| Ack::new(ACK_ERROR_NO_EXIST, "clear", "no active profile"))?;
            queue::clear(&pool)
                .await
                .map_err(|e| ack_arg("clear", &e.to_string()))?;
            drop(pool);
            crate::commands::player::emit_queue_changed(&ctx.app);
            ctx.queue_changed();
            Ok(Response::new())
        }

        Command::Delete(range) => {
            let state = ctx.app.state::<AppState>();
            let pool = state
                .require_profile_pool()
                .await
                .map_err(|_| Ack::new(ACK_ERROR_NO_EXIST, "delete", "no active profile"))?;
            let len = queue::queue_length(&pool).await.unwrap_or(0) as u32;
            let Some((start, end)) = range.resolve(len) else {
                return Err(ack_arg("delete", "Bad song index"));
            };
            queue::remove_range(&pool, start as i64, end as i64)
                .await
                .map_err(|e| ack_arg("delete", &e.to_string()))?;
            drop(pool);
            crate::commands::player::emit_queue_changed(&ctx.app);
            ctx.queue_changed();
            Ok(Response::new())
        }

        Command::DeleteId(id) => {
            let state = ctx.app.state::<AppState>();
            let pool = state
                .require_profile_pool()
                .await
                .map_err(|_| Ack::new(ACK_ERROR_NO_EXIST, "deleteid", "no active profile"))?;
            let removed = queue::remove_by_queue_id(&pool, id as i64)
                .await
                .map_err(|e| ack_arg("deleteid", &e.to_string()))?;
            drop(pool);
            if !removed {
                return Err(Ack::new(ACK_ERROR_NO_EXIST, "deleteid", "No such song"));
            }
            crate::commands::player::emit_queue_changed(&ctx.app);
            ctx.queue_changed();
            Ok(Response::new())
        }

        Command::Move { from, to } => {
            let state = ctx.app.state::<AppState>();
            let pool = state
                .require_profile_pool()
                .await
                .map_err(|_| Ack::new(ACK_ERROR_NO_EXIST, "move", "no active profile"))?;
            let len = queue::queue_length(&pool).await.unwrap_or(0) as u32;
            // Only a single-position move is supported; MPD allows a
            // range but WaveFlow's `reorder` moves one row and building
            // range-move on top of it would need its own transaction.
            let Range::Position(from) = from else {
                return Err(ack_arg("move", "range move is not supported"));
            };
            if from >= len || to >= len {
                return Err(ack_arg("move", "Bad song index"));
            }
            queue::reorder(&pool, from as i64, to as i64)
                .await
                .map_err(|e| ack_arg("move", &e.to_string()))?;
            drop(pool);
            crate::commands::player::emit_queue_changed(&ctx.app);
            ctx.queue_changed();
            Ok(Response::new())
        }

        Command::MoveId { id, to } => {
            let state = ctx.app.state::<AppState>();
            let pool = state
                .require_profile_pool()
                .await
                .map_err(|_| Ack::new(ACK_ERROR_NO_EXIST, "moveid", "no active profile"))?;
            let len = queue::queue_length(&pool).await.unwrap_or(0) as u32;
            let Ok(Some(from)) = queue::position_of_queue_id(&pool, id as i64).await else {
                return Err(Ack::new(ACK_ERROR_NO_EXIST, "moveid", "No such song"));
            };
            if to >= len {
                return Err(ack_arg("moveid", "Bad song index"));
            }
            queue::reorder(&pool, from, to as i64)
                .await
                .map_err(|e| ack_arg("moveid", &e.to_string()))?;
            drop(pool);
            crate::commands::player::emit_queue_changed(&ctx.app);
            ctx.queue_changed();
            Ok(Response::new())
        }

        Command::Shuffle => {
            let state = ctx.app.state::<AppState>();
            let pool = state
                .require_profile_pool()
                .await
                .map_err(|_| Ack::new(ACK_ERROR_NO_EXIST, "shuffle", "no active profile"))?;
            queue::shuffle(&pool)
                .await
                .map_err(|e| ack_arg("shuffle", &e.to_string()))?;
            drop(pool);
            crate::commands::player::emit_queue_changed(&ctx.app);
            ctx.queue_changed();
            Ok(Response::new())
        }

        Command::Random(on) => {
            let state = ctx.app.state::<AppState>();
            let pool = state
                .require_profile_pool()
                .await
                .map_err(|_| Ack::new(ACK_ERROR_NO_EXIST, "random", "no active profile"))?;
            queue::write_shuffle(&pool, on)
                .await
                .map_err(|e| ack_arg("random", &e.to_string()))?;
            let result = if on {
                queue::shuffle(&pool).await
            } else {
                queue::unshuffle(&pool).await
            };
            result.map_err(|e| ack_arg("random", &e.to_string()))?;
            crate::commands::player::emit_options_changed(&ctx.app, &pool).await;
            drop(pool);
            crate::commands::player::emit_queue_changed(&ctx.app);
            ctx.idle.notify(Subsystem::Options);
            ctx.queue_changed();
            Ok(Response::new())
        }

        Command::Repeat(on) => {
            let state = ctx.app.state::<AppState>();
            let pool = state
                .require_profile_pool()
                .await
                .map_err(|_| Ack::new(ACK_ERROR_NO_EXIST, "repeat", "no active profile"))?;
            let current = queue::read_repeat_mode(&pool).await;
            queue::write_repeat_mode(&pool, repeat_from_mpd(current, on))
                .await
                .map_err(|e| ack_arg("repeat", &e.to_string()))?;
            // Reflect the change in the WaveFlow UI too (an MPD client toggled
            // it, so the frontend has no optimistic update of its own).
            crate::commands::player::emit_options_changed(&ctx.app, &pool).await;
            ctx.idle.notify(Subsystem::Options);
            Ok(Response::new())
        }

        Command::Single(mode) => {
            let state = ctx.app.state::<AppState>();
            let pool = state
                .require_profile_pool()
                .await
                .map_err(|_| Ack::new(ACK_ERROR_NO_EXIST, "single", "no active profile"))?;
            let current = queue::read_repeat_mode(&pool).await;
            // `oneshot` means "stop after this track, then clear the
            // flag". WaveFlow has no one-shot variant, so it maps to the
            // closest durable state rather than being rejected.
            let on = matches!(mode, SingleMode::On | SingleMode::OneShot);
            queue::write_repeat_mode(&pool, single_from_mpd(current, on))
                .await
                .map_err(|e| ack_arg("single", &e.to_string()))?;
            crate::commands::player::emit_options_changed(&ctx.app, &pool).await;
            ctx.idle.notify(Subsystem::Options);
            Ok(Response::new())
        }

        Command::Consume(on) => {
            if on {
                // Accepting silently would be a lie: entries would not
                // vanish as they play and the client would show a mode
                // that isn't in effect.
                Err(ack_arg("consume", "consume mode is not supported"))
            } else {
                Ok(Response::new())
            }
        }

        // `idle` is handled by the connection layer, which owns the socket
        // and can hold it open. Reaching here means a command list contained
        // `idle`, which MPD forbids.
        Command::Idle(_) => Err(ack_arg("idle", "idle is not allowed in a list")),

        // A bare `noidle` with no `idle` in flight is a no-op in MPD — answer
        // OK rather than erroring. (When `idle` IS in flight the connection
        // layer consumes the `noidle` and never dispatches it.)
        Command::NoIdle => Ok(Response::new()),

        // A recognized verb whose argument was malformed: MPD answers
        // ACK_ERROR_ARG (2), distinct from an unknown command's ERROR (5).
        Command::BadArgs(verb) => Err(ack_arg(&verb, "invalid argument")),

        Command::Unknown(verb) => Err(Ack::new(
            super::protocol::ACK_ERROR_UNKNOWN,
            &verb,
            format!("unknown command \"{verb}\""),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_maps_onto_mpds_two_flags() {
        assert_eq!(repeat_flags(RepeatMode::Off), (false, false));
        assert_eq!(repeat_flags(RepeatMode::All), (true, false));
        // "Repeat one" is `repeat 1` + `single 1` in MPD.
        assert_eq!(repeat_flags(RepeatMode::One), (true, true));
    }

    #[test]
    fn toggling_repeat_preserves_single() {
        // The client only touched the repeat bit, so a queue already in
        // "repeat one" must not silently downgrade to "repeat all".
        assert_eq!(repeat_from_mpd(RepeatMode::One, true), RepeatMode::One);
        assert_eq!(repeat_from_mpd(RepeatMode::Off, true), RepeatMode::All);
        assert_eq!(repeat_from_mpd(RepeatMode::One, false), RepeatMode::Off);
    }

    #[test]
    fn toggling_single_preserves_repeat() {
        assert_eq!(single_from_mpd(RepeatMode::All, true), RepeatMode::One);
        // Turning single off from "repeat one" leaves repeat on, which
        // is what MPD's own flags do.
        assert_eq!(single_from_mpd(RepeatMode::One, false), RepeatMode::All);
        assert_eq!(single_from_mpd(RepeatMode::Off, false), RepeatMode::Off);
    }

    #[test]
    fn repeat_flags_round_trip_through_both_setters() {
        for mode in [RepeatMode::Off, RepeatMode::All, RepeatMode::One] {
            let (repeat, single) = repeat_flags(mode);
            let after_repeat = repeat_from_mpd(mode, repeat);
            let after_both = single_from_mpd(after_repeat, single);
            assert_eq!(after_both, mode, "{mode:?} did not survive a round trip");
        }
    }

    #[test]
    fn seek_targets_keep_their_fractional_seconds() {
        // The bug this guards: `seconds as u64 * 1000` floors first, so
        // a half-second seek target collapsed to the whole second.
        assert_eq!(seconds_to_ms(12.5), 12_500);
        assert_eq!(seconds_to_ms(0.25), 250);
        assert_eq!(seconds_to_ms(0.0), 0);
        // MPD never sends a negative absolute target, but clamping
        // beats wrapping to u64::MAX if one arrives.
        assert_eq!(seconds_to_ms(-5.0), 0);
    }

    #[test]
    fn advertised_commands_are_sorted_and_unique() {
        // `commands` output feeds client capability detection; a
        // duplicate would be harmless but signals a bad merge.
        let mut sorted = SUPPORTED_COMMANDS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.as_slice(), SUPPORTED_COMMANDS);
    }
}

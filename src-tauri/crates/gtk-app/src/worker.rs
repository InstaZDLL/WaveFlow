//! All database, artwork and playback operations live on the Rust worker.
//! Only owned data crosses back to the GTK main context.
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::mpsc;
use waveflow_core::domain::{playlist::Playlist, track::TrackRow};
use waveflow_native::backend::{Artwork, Backend, Playback, TrackView};

pub enum Command {
    Tracks {
        generation: u64,
        view: TrackView,
        query: String,
    },
    Playlists {
        generation: u64,
    },
    Play {
        ids: Vec<i64>,
        index: usize,
    },
    Toggle,
    Previous,
    Next,
    Seek(u64),
    Volume(f32),
    Shutdown,
}

pub enum Update {
    Stopped,
    Ready {
        profile: String,
        root: PathBuf,
    },
    Tracks {
        generation: u64,
        rows: Vec<TrackRow>,
    },
    Playlists {
        generation: u64,
        rows: Vec<Playlist>,
    },
    Playback(Playback),
    Current {
        row: Option<Box<TrackRow>>,
        artwork: Option<Artwork>,
    },
    Error(String),
}

pub fn start() -> (mpsc::Sender<Command>, mpsc::Receiver<Update>) {
    let (commands, mut rx) = mpsc::channel(64);
    let (updates, ui) = mpsc::channel(64);
    let failures = updates.clone();
    let spawned = std::thread::Builder::new().name("waveflow-native".into()).spawn(move || {
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(err) => { let _ = updates.blocking_send(Update::Error(err.to_string())); return; }
        };
        runtime.block_on(async move {
            // Useful for isolated development/smoke testing; normal startup shares
            // the same XDG data path and default profile as the existing app.
            let root = std::env::var_os("WAVEFLOW_DATA_ROOT").map(PathBuf::from);
            let (backend, mut events) = match Backend::open(root).await {
                Ok(backend) => backend,
                Err(err) => { let _ = updates.send(Update::Error(err.to_string())).await; return; }
            };
            let backend = Arc::new(backend);
            let profile = match backend.active_profile_name().await {
                Ok(name) => name,
                Err(err) => err.to_string(),
            };
            let _ = updates
                .send(Update::Ready {
                    profile,
                    root: backend.data_root().to_path_buf(),
                })
                .await;
            current(&backend, &updates).await;
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut last_id = backend.playback().track_id;
            // Abort obsolete library queries instead of letting a fast typist
            // create an unbounded backlog of SQLite work.
            let mut read_task: Option<tokio::task::JoinHandle<()>> = None;
            loop {
                tokio::select! {
                    command = rx.recv() => {
                        let Some(command) = command else { break };
                        let result = match command {
                            Command::Tracks { generation, view, query } => {
                                if let Some(task) = read_task.take() { task.abort(); }
                                let backend = backend.clone();
                                let updates = updates.clone();
                                read_task = Some(tokio::spawn(async move {
                                    let update = match backend.tracks(&view, &query).await {
                                        Ok(rows) => Update::Tracks { generation, rows },
                                        Err(err) => Update::Error(err.to_string()),
                                    };
                                    let _ = updates.send(update).await;
                                }));
                                Ok(())
                            }
                            Command::Playlists { generation } => {
                                if let Some(task) = read_task.take() { task.abort(); }
                                let backend = backend.clone();
                                let updates = updates.clone();
                                read_task = Some(tokio::spawn(async move {
                                    let update = match backend.playlists().await {
                                        Ok(rows) => Update::Playlists { generation, rows },
                                        Err(err) => Update::Error(err.to_string()),
                                    };
                                    let _ = updates.send(update).await;
                                }));
                                Ok(())
                            }
                            Command::Play { ids, index } => backend.play(&ids, index).await,
                            Command::Toggle => backend.toggle().await,
                            Command::Previous => { backend.previous().await; Ok(()) }
                            Command::Next => { backend.next().await; Ok(()) }
                            Command::Seek(ms) => backend.seek(ms),
                            Command::Volume(value) => backend.set_volume(value).await,
                            Command::Shutdown => break,
                        };
                        if let Err(err) = result { let _ = updates.send(Update::Error(err.to_string())).await; }
                    }
                    _ = tick.tick() => {
                        let playback = backend.playback();
                        if playback.track_id != last_id {
                            last_id = playback.track_id;
                            current(&backend, &updates).await;
                        }
                        if updates.send(Update::Playback(playback)).await.is_err() { break; }
                    }
                    event = events.recv() => {
                        match event {
                            Ok(event) if event.name == "player:error" => {
                                let message = event.payload["message"].as_str().unwrap_or("Erreur de lecture");
                                let _ = updates.send(Update::Error(message.to_owned())).await;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                // Subscribe-before-snapshot plus recovery if the UI fell behind.
                                current(&backend, &updates).await;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            _ => {}
                        }
                    }
                }
            }
            if let Some(task) = read_task { task.abort(); }
            if let Err(err) = backend.shutdown().await { eprintln!("WaveFlow shutdown: {err}"); }
            let _ = updates.send(Update::Stopped).await;
        });
        runtime.shutdown_timeout(Duration::from_secs(3));
    });
    if let Err(err) = spawned {
        let _ = failures.try_send(Update::Error(err.to_string()));
    }
    (commands, ui)
}

async fn current(backend: &Backend, updates: &mpsc::Sender<Update>) {
    match backend.current_track().await {
        Ok(row) => {
            let artwork = match row.as_ref() {
                Some(row) => match backend.artwork(row).await {
                    Ok(artwork) => artwork,
                    Err(err) => {
                        let _ = updates.send(Update::Error(err.to_string())).await;
                        None
                    }
                },
                None => None,
            };
            let _ = updates
                .send(Update::Current {
                    row: row.map(Box::new),
                    artwork,
                })
                .await;
        }
        Err(err) => {
            let _ = updates.send(Update::Error(err.to_string())).await;
        }
    }
}

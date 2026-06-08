use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::{
    audio::{engine::AudioCmd, AudioEngine},
    commands::player::QueueTrackPayload,
    error::{AppError, AppResult},
};

#[derive(Debug, Clone, Copy)]
struct StationDef {
    id: i64,
    slug: &'static str,
    name: &'static str,
    tagline: &'static str,
    genre: &'static str,
    stream_url: &'static str,
    codec_hint: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebRadioStation {
    pub id: i64,
    pub slug: String,
    pub name: String,
    pub tagline: String,
    pub genre: String,
    pub codec: String,
}

const STATIONS: &[StationDef] = &[
    StationDef {
        id: 1,
        slug: "groove-salad",
        name: "Groove Salad",
        tagline: "Ambient and downtempo beats",
        genre: "Chillout",
        stream_url: "https://ice5.somafm.com/groovesalad-128-mp3",
        codec_hint: "mp3",
    },
    StationDef {
        id: 2,
        slug: "drone-zone",
        name: "Drone Zone",
        tagline: "Atmospheric textures for deep focus",
        genre: "Ambient",
        stream_url: "https://ice5.somafm.com/dronezone-128-mp3",
        codec_hint: "mp3",
    },
    StationDef {
        id: 3,
        slug: "beat-blender",
        name: "Beat Blender",
        tagline: "Late-night downtempo and instrumental grooves",
        genre: "Electronic",
        stream_url: "https://ice5.somafm.com/beatblender-128-mp3",
        codec_hint: "mp3",
    },
    StationDef {
        id: 4,
        slug: "def-con-radio",
        name: "DEF CON Radio",
        tagline: "Hacker culture beats and dark electronics",
        genre: "Electronic",
        stream_url: "https://ice5.somafm.com/defcon-128-mp3",
        codec_hint: "mp3",
    },
    StationDef {
        id: 5,
        slug: "secret-agent",
        name: "Secret Agent",
        tagline: "The soundtrack for stylish missions",
        genre: "Lounge",
        stream_url: "https://ice5.somafm.com/secretagent-128-mp3",
        codec_hint: "mp3",
    },
];

#[tauri::command]
pub fn web_radio_list_stations() -> Vec<WebRadioStation> {
    STATIONS.iter().map(station_to_payload).collect()
}

#[tauri::command]
pub fn web_radio_play_station(
    app: AppHandle,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    station_id: i64,
) -> AppResult<()> {
    let station = STATIONS
        .iter()
        .find(|station| station.id == station_id)
        .ok_or_else(|| AppError::Other(format!("unknown web radio station {station_id}")))?;
    let track_id = synthetic_track_id(station.id);
    let payload = QueueTrackPayload {
        id: track_id,
        title: station.name.to_string(),
        artist_id: None,
        artist_name: Some("Web Radio".to_string()),
        artist_ids: None,
        album_title: Some(station.genre.to_string()),
        duration_ms: 0,
        file_path: station.stream_url.to_string(),
        artwork_path: None,
        artwork_path_1x: None,
        artwork_path_2x: None,
        bitrate: Some(128_000),
        sample_rate: None,
        channels: None,
        bit_depth: None,
        codec: Some("MP3 stream".to_string()),
        file_size: 0,
    };

    let _ = app.emit("player:track-changed", payload.clone());
    if let Some(tray) = app.tray_by_id("waveflow") {
        let _ = tray.set_tooltip(Some(format!("{} - Web Radio", station.name)));
    }
    if let Some(controls) = app.try_state::<crate::media_controls::MediaControlsHandle>() {
        controls.update_metadata(
            payload.title,
            payload.artist_name,
            payload.album_title,
            None,
            0,
        );
    }

    engine.send(AudioCmd::LoadUrlAndPlay {
        url: station.stream_url.to_string(),
        codec_hint: Some(station.codec_hint.to_string()),
        track_id,
        source_type: "web-radio".to_string(),
        source_id: Some(station.id),
    })
}

fn station_to_payload(station: &StationDef) -> WebRadioStation {
    WebRadioStation {
        id: station.id,
        slug: station.slug.to_string(),
        name: station.name.to_string(),
        tagline: station.tagline.to_string(),
        genre: station.genre.to_string(),
        codec: station.codec_hint.to_uppercase(),
    }
}

fn synthetic_track_id(station_id: i64) -> i64 {
    -10_000 - station_id
}

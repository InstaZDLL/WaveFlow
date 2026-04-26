//! Tauri commands exposed to the frontend.
//!
//! Commands are grouped by domain. Each submodule declares the types shared
//! with the frontend via `serde`, plus the `#[tauri::command]` entry points.

pub mod app_info;
pub mod browse;
pub mod deezer;
pub mod integration;
pub mod library;
pub mod lyrics;
pub mod player;
pub mod playlist;
pub mod profile;
pub mod scan;
pub mod stats;
pub mod track;

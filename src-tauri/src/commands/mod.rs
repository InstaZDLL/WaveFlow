//! Tauri commands exposed to the frontend.
//!
//! Commands are grouped by domain. Each submodule declares the types shared
//! with the frontend via `serde`, plus the `#[tauri::command]` entry points.

pub mod app_info;
pub mod browse;
pub mod library;
pub mod profile;
pub mod scan;
pub mod track;

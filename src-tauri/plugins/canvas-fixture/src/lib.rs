//! WaveFlow Canvas-world test fixture — guest side.
//!
//! A deliberately tiny `waveflow:canvas/v1` plugin whose only job is to
//! let the host's integration test exercise the Canvas world end-to-end.
//! Its `track-canvas` is a pure function of the inputs (no network):
//!
//! - empty `title`            → `Ok(none)` (the "no Canvas" path)
//! - `title == "boom"`        → `Err(...)` (the provider-failure path)
//! - anything else            → `Ok(some(canvas))` with a URL + entity-id
//!   echoing the args, so the test can assert they round-tripped.
//!
//! This crate is NOT shipped — see `Cargo.toml`.

#[allow(warnings)]
mod bindings;

use bindings::exports::waveflow::canvas::provider::{Canvas, Guest};

struct Fixture;

impl Guest for Fixture {
    fn track_canvas(
        artist: String,
        title: String,
        _album: Option<String>,
        _duration_ms: Option<u32>,
    ) -> Result<Option<Canvas>, String> {
        if title.is_empty() {
            return Ok(None);
        }
        if title == "boom" {
            return Err("provider failure".to_string());
        }
        Ok(Some(Canvas {
            url: format!("https://example.com/canvas/{artist}/{title}.mp4"),
            entity_id: Some(format!("id:{title}")),
        }))
    }
}

bindings::export!(Fixture with_types_in bindings);

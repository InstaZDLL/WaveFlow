//! Audio file-format helpers that aren't tied to the desktop's
//! real-time `cpal` pipeline.
//!
//! For now: the in-tree DSD (Direct Stream Digital) parser, PCM
//! converter, and metadata reader. Symphonia doesn't decode DSD
//! natively (1-bit @ 2.8 / 5.6 / 11.2 / 22.5 MHz), so the scanner
//! ingests DSF / DFF tracks via this module and the desktop's
//! crossfade engine streams them through the same PCM converter.
//! The future `waveflow-server` will reuse the same code path when
//! transcoding DSD uploads.

pub mod dsd;

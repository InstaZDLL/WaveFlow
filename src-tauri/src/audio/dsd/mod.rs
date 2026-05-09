//! Direct Stream Digital (DSD) support.
//!
//! Symphonia 0.5 doesn't decode DSD natively (1-bit @ 2.8 / 5.6 / 11.2
//! / 22.5 MHz), so this module owns the whole pipeline:
//!
//!   - [`parser`] reads the two on-disk container formats: **DSF**
//!     (Sony, chunk-based) and **DFF** (Philips, IFF/FORM-style),
//!     extracts the bitstream layout (sample rate, channels, total
//!     sample count), and locates the data chunk for streaming.
//!   - [`pcm`] converts the 1-bit DSD stream to 24-bit PCM via a
//!     decimating FIR low-pass. The filter window is hardcoded
//!     (Blackman-Harris) and the decimation factor is chosen so the
//!     output rate lands on a Symphonia-friendly 88.2 kHz / 96 kHz
//!     range that the rest of the audio engine already handles.
//!   - [`metadata`] reads the sidecar metadata embedded in the
//!     container (DSF carries an ID3v2 chunk in its footer, DFF uses
//!     its own DIIN/COMT/CMNT atoms) so the scanner can populate
//!     title / artist / album rows without a second tag library.
//!
//! Étape 1 of the rollout (this commit) ships [`parser`] only, with
//! exhaustive tests against synthesised fixtures. PCM conversion and
//! metadata follow in subsequent commits.

pub mod metadata;
pub mod parser;
pub mod pcm;

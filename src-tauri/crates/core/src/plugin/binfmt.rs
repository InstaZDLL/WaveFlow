//! What kind of wasm binary is this, told from the preamble alone.
//!
//! The host runs **components**, not core modules. The two are
//! different formats that share the same four magic bytes, so a core
//! module looks like a plugin right up until wasmtime refuses it —
//! and what it says then ("failed to parse WebAssembly module") reads
//! like a corrupt file rather than the wrong format.
//!
//! It happened to two published WaveFlow plugins, and the reason is
//! worth recording because it is not the obvious one. Their release
//! workflow ran `cargo component build` correctly and then packaged
//! the wrong output: `cargo component` leaves rustc's intermediate
//! core modules in the same `target/<triple>/release/` tree as the
//! component it produces, all with the same extension, and the
//! packaging step picked one with `find … | head -n1`. A compiler
//! bump reshuffled the filenames in `deps/` and the pick changed. So
//! the mistake to look for is **what was published**, not how it was
//! built.
//!
//! Eight bytes are enough to tell them apart, which means the answer
//! is available at install time, before anything is written to disk,
//! and at list time without paying for a Cranelift compile.

/// The four bytes both formats open with: `\0asm`.
pub const WASM_MAGIC: [u8; 4] = [0x00, 0x61, 0x73, 0x6d];

/// Bytes 4..8 of the preamble, as two little-endian `u16`s:
/// `version` then `layer`. A core module is version 1 / layer 0; a
/// component is layer 1, and its version tracks the component-model
/// revision (0x000d at the time of writing) rather than the core
/// wasm version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmBinaryKind {
    /// Layer 1 — a component. This is what the host can load.
    Component,
    /// Version 1, layer 0 — a core module. Valid wasm, wrong format:
    /// built with `cargo build` instead of `cargo component build`.
    CoreModule,
    /// Correct magic, a preamble neither of the above. Either a wasm
    /// encoding newer than this host knows, or a truncated file.
    UnknownEncoding { version: u16, layer: u16 },
    /// Not wasm at all — the magic is missing or the file is shorter
    /// than a preamble.
    NotWasm,
}

impl WasmBinaryKind {
    /// What the person holding this file can actually do about it.
    ///
    /// The advice has to fit the mistake. Telling someone whose
    /// download landed as an HTML error page to rebuild with
    /// `cargo component build` sends them to look at something that
    /// was never wrong — and so does telling that to someone whose
    /// build was fine and whose packaging step picked the wrong file,
    /// which is what actually happened to the two plugins this module
    /// exists for. So the core-module hint leads with the artifact and
    /// keeps the command only as the thing that produces the right
    /// one — a plain `cargo build` is still a way to get here.
    pub fn load_hint(self) -> &'static str {
        match self {
            Self::Component => "the host loads this format",
            Self::CoreModule => {
                "what was published is not the component `cargo component build` writes: \
                 check the packaging step, since rustc's intermediate modules under \
                 `target/<triple>/release/deps/` share the extension and are easy to pick up by mistake"
            }
            Self::UnknownEncoding { .. } => {
                "the host cannot load this encoding; the plugin may need a newer WaveFlow, \
                 or the file may be truncated"
            }
            Self::NotWasm => {
                "the host cannot load this file; the download may be incomplete, or the \
                 wrong asset may have been published under this name"
            }
        }
    }
}

impl std::fmt::Display for WasmBinaryKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Component => f.write_str("a WebAssembly component"),
            Self::CoreModule => f.write_str("a core WebAssembly module, not a component"),
            Self::UnknownEncoding { version, layer } => write!(
                f,
                "a WebAssembly binary in an unknown encoding (version {version}, layer {layer})"
            ),
            Self::NotWasm => f.write_str("not a WebAssembly binary"),
        }
    }
}

/// Classify a `plugin.wasm` from its first eight bytes.
///
/// Only the preamble is read, so this says nothing about whether the
/// rest of the file is valid — a truncated component still answers
/// [`WasmBinaryKind::Component`]. That is the right split of labour:
/// this catches the *format* mistake with a message that names its
/// cause, and wasmtime keeps catching everything else.
///
/// Any layer other than 0 is reported as an unknown encoding rather
/// than guessed at. Layer 1 is the component model; the field exists
/// precisely so future encodings can claim their own number, and
/// calling one of those "a component" would put a load error back
/// where this module is meant to remove one.
pub fn classify_wasm_binary(bytes: &[u8]) -> WasmBinaryKind {
    if bytes.len() < 8 || bytes[..4] != WASM_MAGIC {
        return WasmBinaryKind::NotWasm;
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    let layer = u16::from_le_bytes([bytes[6], bytes[7]]);
    match (version, layer) {
        (_, 1) => WasmBinaryKind::Component,
        (1, 0) => WasmBinaryKind::CoreModule,
        _ => WasmBinaryKind::UnknownEncoding { version, layer },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four preambles measured on disk while diagnosing the
    /// broken plugins: two published plugins that were rebuilt with
    /// the wrong command, and two that were repackaged untouched.
    #[test]
    fn tells_a_component_from_a_core_module() {
        let component = [0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00];
        let core_module = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        assert_eq!(
            classify_wasm_binary(&component),
            WasmBinaryKind::Component,
            "apple-lyrics / spotify-canvas preamble"
        );
        assert_eq!(
            classify_wasm_binary(&core_module),
            WasmBinaryKind::CoreModule,
            "apple-artwork / release-radar preamble"
        );
    }

    /// A component-model revision this host predates is still a
    /// component: the layer, not the version, is what separates the
    /// formats, and mislabelling it would send the user chasing a
    /// build command that was never the problem.
    #[test]
    fn a_newer_component_revision_is_still_a_component() {
        let future = [0x00, 0x61, 0x73, 0x6d, 0xff, 0x00, 0x01, 0x00];
        assert_eq!(classify_wasm_binary(&future), WasmBinaryKind::Component);
    }

    #[test]
    fn an_unknown_layer_is_not_guessed_at() {
        let odd = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x07, 0x00];
        assert_eq!(
            classify_wasm_binary(&odd),
            WasmBinaryKind::UnknownEncoding {
                version: 1,
                layer: 7
            }
        );
    }

    /// Anything that isn't wasm — an HTML error page saved under the
    /// asset's name, a zip, an empty file — answers the same way, and
    /// a file shorter than the preamble must not index out of bounds.
    #[test]
    fn a_non_wasm_file_is_refused_without_panicking() {
        assert_eq!(classify_wasm_binary(b""), WasmBinaryKind::NotWasm);
        assert_eq!(classify_wasm_binary(b"\0asm"), WasmBinaryKind::NotWasm);
        assert_eq!(
            classify_wasm_binary(b"<!DOCTYPE html>"),
            WasmBinaryKind::NotWasm
        );
        assert_eq!(
            classify_wasm_binary(b"PK\x03\x04...."),
            WasmBinaryKind::NotWasm
        );
    }
}

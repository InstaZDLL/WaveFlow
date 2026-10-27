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

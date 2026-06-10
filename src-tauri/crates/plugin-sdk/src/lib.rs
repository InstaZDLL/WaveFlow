//! WaveFlow Plugin SDK — type-level contracts.
//!
//! This crate is the workspace-internal source of truth for the
//! plugin protocol. It is intentionally small: it only declares the
//! manifest schema version + the supported WIT world labels. Both
//! sides of the contract (the host in `waveflow-core::plugin` and
//! the future author-facing `waveflow-plugin` crate) read from here
//! so there's exactly one place to bump when the protocol moves.
//!
//! WIT files live under `wit/`. They are NOT consumed by this crate
//! directly — Phase 2 will wire `wasmtime::component::bindgen!` from
//! `waveflow-core::plugin::runtime` and the eventual `waveflow-plugin`
//! author crate will wire `wit-bindgen` macros. Both will point at
//! `crates/plugin-sdk/wit/` so there is no duplicated source.

/// Plugin manifest schema version. Bumped on every breaking change to
/// `manifest.toml` format. Plugins ship the version they were
/// authored against; the host rejects any mismatch at load time
/// rather than silently misinterpreting fields.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// The three WIT worlds a plugin can export. Each one corresponds to
/// a file under `wit/`. A plugin manifest declares exactly one
/// world; mixing exports is out of scope for v1 of the SDK.
pub mod worlds {
    /// `waveflow:source/v1` — source providers (Web Radio, podcasts,
    /// alternative metadata sources that yield tracks the engine
    /// can play). See `wit/waveflow-source.wit`.
    pub const SOURCE_V1: &str = "waveflow:source/v1";

    /// `waveflow:metadata/v1` — metadata enrichers (Musixmatch,
    /// MusicBrainz, Discogs). Don't return tracks; enrich existing
    /// rows with biographies, similar artists, lyrics. See
    /// `wit/waveflow-metadata.wit`.
    pub const METADATA_V1: &str = "waveflow:metadata/v1";

    /// `waveflow:ui/v1` — UI extensions (custom views, panels). Return
    /// view descriptors the host renders. See `wit/waveflow-ui.wit`.
    pub const UI_V1: &str = "waveflow:ui/v1";

    /// Returns `true` if the label names a world this version of the
    /// SDK knows about. The host uses it to reject manifests
    /// pointing at unknown worlds before instantiating the wasm.
    pub fn is_known(world: &str) -> bool {
        matches!(world, SOURCE_V1 | METADATA_V1 | UI_V1)
    }
}

/// Host permission identifiers (manifest `[permissions]` table).
/// Plugins declare what they need; the host enforces these at the
/// wasm import boundary. Anything not declared is denied by
/// default.
pub mod permissions {
    /// Outbound HTTP. The value declared in the manifest is an
    /// allowlist of host patterns (`https://example.com/*`). The
    /// host validates every URL against the list before dispatching
    /// the `waveflow:host/http` import.
    pub const HTTP: &str = "http";

    /// Read-only access to plugin-bundled sidecar assets (the
    /// `assets/` subdirectory next to `manifest.toml`). Plugins
    /// have NO access to the host's user data, profile DBs, or
    /// arbitrary filesystem locations — this permission only
    /// covers what the plugin author shipped with the manifest.
    pub const STORAGE_READ: &str = "storage.read";

    /// Read-write access to a small per-plugin scratch directory
    /// the host allocates (`~/.config/waveflow/plugin-data/<plugin-id>/`).
    /// Used by plugins that need to persist a small amount of
    /// per-user state (favourites, settings). Capped at 10 MB by
    /// the host; over-quota writes fail.
    pub const STORAGE_WRITE: &str = "storage.write";

    /// Returns `true` if the string names a permission this SDK
    /// version recognises. Unknown permissions in a manifest are
    /// surfaced as a load-time error so a future-permission plugin
    /// doesn't silently get NO access.
    pub fn is_known(perm: &str) -> bool {
        matches!(perm, HTTP | STORAGE_READ | STORAGE_WRITE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_worlds_round_trip() {
        assert!(worlds::is_known(worlds::SOURCE_V1));
        assert!(worlds::is_known(worlds::METADATA_V1));
        assert!(worlds::is_known(worlds::UI_V1));
        assert!(!worlds::is_known("waveflow:bogus/v1"));
    }

    #[test]
    fn known_permissions_round_trip() {
        assert!(permissions::is_known(permissions::HTTP));
        assert!(permissions::is_known(permissions::STORAGE_READ));
        assert!(permissions::is_known(permissions::STORAGE_WRITE));
        assert!(!permissions::is_known("network.tcp"));
    }
}

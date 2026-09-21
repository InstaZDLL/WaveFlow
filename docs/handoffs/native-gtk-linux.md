# Native GTK Linux frontend handoff

## Goal and non-negotiable constraints

WaveFlow is gaining a Linux-native frontend written in Rust with GTK4 and
libadwaita-rs. The existing React/Tauri application remains supported and the
shared Rust audio, SQLite, migrations, scanner, and business logic must not be
duplicated or rewritten. The GTK frontend must not use Tauri, WebKitGTK, Vala,
a C shim, or handwritten C FFI. GTK objects stay on the main thread; database,
artwork, and audio work is dispatched to a background worker.

The implementation branch is `feat/native-gtk-linux`. It is based on the
upstream `main` branch after pull requests #703 through #707, including the
profile-scoped audio-setting restoration and the reserved `.waveflow/` library
directory invariant.

## Architecture implemented

- `src-tauri/crates/core/` remains the portable domain and persistence crate.
- Reusable pieces formerly coupled to the Tauri host were extracted inside
  `src-tauri/crates/app/src/`: host events/spawning, profile selection and pool
  access, playback gain, scrobble queue state, and Tauri-specific path helpers.
- `src-tauri/crates/native/` provides a pure-Rust native host. It opens and
  migrates the same application/profile databases, selects the last/default
  profile, owns the existing `AudioEngine`, calls shared `player_actions`, and
  exposes library/search/favorites/history/playlists/artwork/player operations.
- `src-tauri/crates/gtk-app/` is a separate Cargo workspace. This separation is
  intentional: the current Tauri Linux dependency graph and gtk4-rs require
  incompatible generations of the GLib `links` crates. The GTK application
  depends on `waveflow-native`, not on Tauri.
- `gtk-app/src/worker.rs` owns a Tokio runtime on an OS worker thread and uses
  bounded channels to return updates to the GTK main loop. Superseded library
  searches are discarded and shutdown is explicit.
- `gtk-app/src/ui.rs` implements the libadwaita window, native sidebar,
  library/search and playlist views, empty/error states and toasts, plus a
  persistent player bar with artwork, play/pause, previous/next, seek, and
  volume controls. Track activation starts real playback through the shared
  Rust engine.
- The Flatpak manifest now builds the native GTK binary offline and installs it
  as `/app/bin/waveflow`. Generated GTK Cargo sources are kept separately from
  the root-workspace Cargo sources.

No existing migration was edited and the Tauri frontend/codepath was retained.

## Important files

- `src-tauri/crates/native/src/backend.rs`: native service API used by GTK.
- `src-tauri/crates/native/src/state.rs`: DB/profile initialization and tests.
- `src-tauri/crates/gtk-app/src/ui.rs`: GTK/libadwaita presentation.
- `src-tauri/crates/gtk-app/src/worker.rs`: non-blocking worker boundary.
- `src-tauri/crates/app/src/host.rs`: Tauri adapter for shared app modules.
- `src-tauri/crates/app/src/player_actions.rs`: shared playback actions.
- `packaging/flatpak/app.waveflow.WaveFlow.yaml`: native Linux package build.
- `packaging/flatpak/generate-gtk-sources.sh`: reproducible source generation.
- `docs/architecture/crates.md`: workspace and host-boundary documentation.

## Validation completed

The following passed during this increment:

- root and GTK `cargo fmt --check`;
- root workspace `cargo check --workspace --all-targets --offline`;
- root workspace Clippy with `-D warnings`;
- full root workspace tests (653 app, 351 core, 247 native tests plus plugin,
  SDK, synced-lyrics, and doc-test suites); local-socket tests were rerun with
  normal host permissions and passed;
- GTK workspace Clippy and tests with `--locked --offline -D warnings`;
- the exact Flatpak release Cargo command with `--release --locked --offline`;
- `bun run typecheck` and `bun run lint`;
- Flatpak source coverage plus its 21 self-tests;
- desktop file, AppStream, and YAML validation;
- dependency-tree/source checks confirming no Tauri, WebKitGTK, GTK3, Vala,
  bindgen/cbindgen, `extern "C"`, or handwritten FFI in the native frontend;
- a release-binary smoke run under Xvfb for eight seconds. It remained alive
  until the intentional timeout and created/migrated a fresh temporary profile.

`flatpak-builder` was not installed in the development environment, so a full
sandboxed Flatpak assembly was not run. The exact locked/offline release build
and all manifest/source validators did pass.

## Remaining work / recommended next increments

1. Run a full `flatpak-builder` build in an environment with the GNOME runtime
   already installed, then capture a GTK screenshot for the pull request.
2. Extract scanner orchestration and progress events behind the native host
   boundary, then add a native folder picker and scan UI. Preserve the task
   registry, single-writer SQLite rule, and `.waveflow/` reserved-directory
   invariant. The current GTK increment reads the real existing library but
   does not initiate a scan.
3. Add a visible profile chooser. The native host currently selects the last
   active profile or creates/uses the default profile correctly.
4. Port native MPRIS and notifications before restoring their Flatpak
   permissions. Network/Discord/tray integrations are also intentionally not
   exposed by the first GTK increment.
5. Connect GTK strings to WaveFlow's localization catalog and complete lyrics
   presentation. Artwork loading is already wired.
6. Expand the minimal Home and Settings pages, add focused UI tests, and perform
   keyboard/screen-reader testing on a real GNOME session.

Before further extraction, reread `AGENTS.md`, `CLAUDE.md`, and
`docs/architecture/invariants.md`. In particular, do not touch existing
migrations, do not block the GTK loop, and keep the audio callback allocation-,
lock-, and log-free.

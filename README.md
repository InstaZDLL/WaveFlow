<p align="center">
  <img src="assets/logo.svg" width="80" alt="WaveFlow logo" />
</p>

<h1 align="center">WaveFlow</h1>

<p align="center">
  <strong>Local music player for desktop — built with Tauri 2, React 19 & Rust</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/version-0.1.0-emerald?style=flat-square" alt="Version" />
  <img src="https://img.shields.io/badge/tauri-2.11-blue?style=flat-square&logo=tauri" alt="Tauri 2" />
  <img src="https://img.shields.io/badge/react-19-61dafb?style=flat-square&logo=react" alt="React 19" />
  <img src="https://img.shields.io/badge/rust-stable-orange?style=flat-square&logo=rust" alt="Rust" />
  <img src="https://img.shields.io/badge/license-GPL--3.0-green?style=flat-square" alt="License" />
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey?style=flat-square" alt="Platform" />
</p>

---

WaveFlow is a local music player desktop app with a Spotify-inspired 3-panel UI. It scans your local audio folders, organizes tracks by album/artist/genre, and plays them with a real-time audio engine — no streaming, no cloud, your music stays on your machine.

## Download

Pre-built bundles for every tagged release are on the [GitHub Releases page](https://github.com/InstaZDLL/WaveFlow/releases/latest). Pick the one that matches your environment:

### Linux

- `WaveFlow_<ver>_linux-x86_64.deb` — Debian / Ubuntu / Mint / Pop!\_OS. Native install via `apt`/`dpkg`, integrates with the system menu.
- `WaveFlow_<ver>_linux-x86_64.rpm` — Fedora / RHEL / openSUSE / Rocky / Alma. Native install via `dnf`/`rpm`.
- `WaveFlow_<ver>_linux-x86_64.AppImage` — Anything else (Calculate Linux, Oracle Linux, NixOS, …). `chmod +x` then run; no install required.
- **Arch / Manjaro / EndeavourOS** — install `waveflow-bin` from the AUR: `yay -S waveflow-bin` (or `paru`, etc.). Tracks the `.deb` artefact above.

### Windows

- `WaveFlow_<ver>_windows-x86_64-setup.exe` — NSIS installer, **per-user** install under `%LOCALAPPDATA%`, doesn't need admin. This is what the in-app updater patches.
- `WaveFlow_<ver>_windows-x86_64.msi` — MSI installer, **system-wide** install under `Program Files`, suitable for IT deployment via GPO/SCCM. Requires admin.

Both are Authenticode-signed. SmartScreen may still warn the first few users while a fresh certificate accumulates reputation — click **More info → Run anyway**.

### macOS

- `WaveFlow_<ver>_macos-universal.dmg` — Intel + Apple Silicon in one bundle.

The macOS build is **not Apple-Developer-signed yet**, so Gatekeeper will block the first launch:

- macOS 14 (Sonoma) and earlier: **right-click the app → Open**, confirm the dialog.
- macOS 15 (Sequoia) and later: launch normally, then go to **System Settings → Privacy & Security** and click **Open Anyway** next to the blocked app.
- Terminal escape hatch (any version): `xattr -cr /Applications/WaveFlow.app`

### Auto-updates

Once installed (any of the above), the in-app updater fetches future versions automatically — but only the AppImage and the NSIS setup are auto-updatable. DEB / RPM / MSI are managed by their respective package managers and stay on the version you installed until you upgrade them externally.

## Features

| Area                | Highlights                                                                                                                                                                                                            | Deep dive                                |
| ------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------- |
| **Playback**        | Symphonia + cpal, lock-free 3-thread engine, real dual-decoder crossfade, ReplayGain, output-device picker, OS media controls (SMTC / MPRIS / MediaRemote), persistent queue with shuffle / repeat / auto-advance     | [docs](docs/features/playback.md)        |
| **Library**         | Folder scanning + filesystem watcher, on-demand audio analysis (peak, loudness, ReplayGain, BPM), Hi-Res badges, multi-artist split, POPM 5-star ratings, A-Z navigator, multi-select action bar                      | [docs](docs/features/library.md)         |
| **Playlists**       | Drag-and-drop reorder (virtualised), bulk add from any source, M3U import / export with basename-fallback matching, likes, recently-played                                                                            | [docs](docs/features/playlists.md)       |
| **Smart playlists** | Auto-generated **Daily Mix** family bucketed by tempo, with composite artist-photo covers rendered from your Deezer cache                                                                                             | [docs](docs/features/smart-playlists.md) |
| **Integrations**    | Deezer (artwork + labels), Last.fm (bios + scrobbling with retry queue), LRCLIB (synchronised lyrics), Discord Rich Presence ("Listening to WaveFlow" with cover + progress bar) — all cached locally for offline use | [docs](docs/features/integrations.md)    |
| **UI & UX**         | Spotify-style 3-panel layout, system tray, statistics dashboard, virtual scroll for 6000+ tracks, dark mode (View Transitions API), 17 locales (RTL-aware), per-profile isolated DB, signed auto-updater              | [docs](docs/features/ui.md)              |

## Tech Stack

| Layer                     | Technologies                                                                                                                       |
| ------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| **Desktop shell**         | Tauri 2.10 (tray icon, opener, dialog, updater plugins)                                                                            |
| **OS media controls**     | souvlaki 0.8 (SMTC / MPRIS / MediaRemote bridge)                                                                                   |
| **Discord Rich Presence** | discord-rich-presence 1.1 (local IPC named pipe, no auth)                                                                          |
| **Frontend**              | React 19, TypeScript, Vite 8, Tailwind CSS 4, Lucide icons, `@dnd-kit` (drag-and-drop), `@tanstack/react-virtual` (virtualization) |
| **Backend**               | Rust, SQLite (sqlx), FTS5 contentless full-text search                                                                             |
| **Audio**                 | symphonia 0.5 (decode), cpal 0.17 (output), rubato 2.0 (resample), rtrb 0.3 (SPSC ring)                                            |
| **Metadata extraction**   | lofty 0.24 (tags, embedded art, POPM, INITIALKEY)                                                                                  |
| **Imaging**               | image 0.25 + fast_image_resize 6 (SIMD thumbnails)                                                                                 |
| **Filesystem watcher**    | notify 8 (debounced rescans of watched folders)                                                                                    |
| **External APIs**         | Deezer public API (no auth) + Last.fm (read + signed methods via md-5 + reqwest 0.12 with rustls) + LRCLIB (synchronized lyrics)   |
| **Package manager**       | Bun                                                                                                                                |

## Getting Started

```bash
# Install dependencies
bun install

# Run the desktop app in development mode
bun run tauri dev

# Build for production
bun run tauri build
```

## Development Commands

```bash
bun run dev          # Vite dev server only (no Tauri shell)
bun run typecheck    # TypeScript check
bun run lint         # ESLint
bun run lint:fix     # ESLint with auto-fix
bun run format       # Prettier

# Rust backend
cargo check --manifest-path src-tauri/Cargo.toml --all-targets
cargo test  --manifest-path src-tauri/Cargo.toml
```

## Documentation

Per-feature deep dives, architecture and storage layout live under [`docs/`](docs/README.md):

- **Features** — [playback](docs/features/playback.md) · [library](docs/features/library.md) · [playlists](docs/features/playlists.md) · [smart playlists](docs/features/smart-playlists.md) · [integrations](docs/features/integrations.md) · [UI & UX](docs/features/ui.md)
- **Architecture** — [audio engine](docs/architecture/audio.md) · [database & paths](docs/architecture/storage.md)
- **Contributing** — [CONTRIBUTING.md](CONTRIBUTING.md) · [RELEASING.md](RELEASING.md)

## License

```
Copyright (C) 2026 InstaZDLL

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
GNU General Public License for more details.

You should have received a copy of the GNU General Public License
along with this program. If not, see <https://www.gnu.org/licenses/>.
```

See [LICENSE](LICENSE) for the full text.

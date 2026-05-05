# Releasing WaveFlow

This document covers the **first-release setup** (one-time keypair
generation) and the **per-release process** (build, sign, publish).

## One-time setup

WaveFlow ships auto-updates via `tauri-plugin-updater`. Updates are
**signed** with a minisign keypair you control: the **private key**
signs each release, the **public key** is embedded in the app and
verifies signatures at install time. Without this you cannot ship
patches without users reinstalling manually.

### 1. Generate the keypair

```sh
bun run tauri signer generate -w ~/.tauri/waveflow.key
```

This produces:

- `~/.tauri/waveflow.key` — **private key**. Keep it secret. Back it
  up somewhere safe (1Password, hardware key, encrypted USB stick).
  **Losing it means you can never ship another signed update for the
  current pubkey** and existing users get stuck on their version.
- `~/.tauri/waveflow.key.pub` — **public key**. Goes into the
  committed config.

You'll be prompted for a password. Use a strong one and store it next
to the private key.

### 2. Embed the public key

Open the public key file, copy its contents, and replace the
placeholder in [`src-tauri/tauri.conf.json`](../src-tauri/tauri.conf.json):

```jsonc
"plugins": {
  "updater": {
    "active": true,
    "endpoints": ["..."],
    "pubkey": "PASTE_THE_PUBLIC_KEY_LINE_HERE"
  }
}
```

The pubkey is a single base64 line starting with `RWQ` or similar.
Commit this change.

### 3. Confirm the endpoint

The default endpoint is GitHub Releases:

```
https://github.com/InstaZDLL/WaveFlow/releases/latest/download/latest.json
```

If you self-host, change it to your manifest URL. The plugin
substitutes `{{target}}`, `{{arch}}`, and `{{current_version}}` in
the URL if you include those placeholders.

## Per-release process

### 1. Bump the version

Edit `src-tauri/tauri.conf.json` → `version` and `package.json` →
`version`. Use semver. Commit:

```sh
git commit -am "chore: release v0.2.0"
git tag v0.2.0
```

### 2. Build & sign

Set the env vars so Tauri picks up your private key:

```sh
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/waveflow.key)"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="<password>"

bun run tauri build
```

This produces signed bundles in `src-tauri/target/release/bundle/`:

- Windows: `nsis/WaveFlow_<ver>_x64-setup.exe` + `.sig`
- macOS: `macos/WaveFlow.app.tar.gz` + `.sig`
- Linux: `appimage/waveflow_<ver>_amd64.AppImage` + `.sig`

(`createUpdaterArtifacts: true` in the bundle config gives you the
`.tar.gz` / `.AppImage` formats the updater needs alongside the
human-installable formats.)

### 3. Write the manifest

`latest.json` describes the release for the updater:

```json
{
  "version": "0.2.0",
  "notes": "Short release notes shown in the update banner.",
  "pub_date": "2026-05-05T20:00:00Z",
  "platforms": {
    "windows-x86_64": {
      "signature": "<contents of WaveFlow_0.2.0_x64-setup.exe.sig>",
      "url": "https://github.com/InstaZDLL/WaveFlow/releases/download/v0.2.0/WaveFlow_0.2.0_x64-setup.exe"
    },
    "darwin-aarch64": {
      "signature": "<contents of .sig>",
      "url": "https://github.com/InstaZDLL/WaveFlow/releases/download/v0.2.0/WaveFlow.app.tar.gz"
    },
    "linux-x86_64": {
      "signature": "<contents of .sig>",
      "url": "https://github.com/InstaZDLL/WaveFlow/releases/download/v0.2.0/waveflow_0.2.0_amd64.AppImage"
    }
  }
}
```

Drop platforms you don't ship.

### 4. Upload to GitHub Releases

Create the release `v0.2.0`, attach the bundle files **and**
`latest.json`. Mark it as the latest release so the
`/releases/latest/download/latest.json` URL resolves.

### 5. Verify

On a machine running the previous version:

1. Wait or restart the app — it checks `latest.json` on launch.
2. The bottom-right banner should appear within seconds.
3. Click "Install now", confirm the OS dialog if any, restart.
4. Help → About should show the new version.

If the banner doesn't appear, check the console (`F12` if devtools
are enabled in the build, or the platform's log directory) for
updater errors. Common causes: pubkey mismatch (private/public
keys regenerated), endpoint 404, malformed `latest.json`,
signature corrupted on upload.

## Notes

- **Dev builds skip the updater entirely** (gated on
  `cfg(not(debug_assertions))` in `lib.rs`). You will not see
  update prompts during `bun run tauri dev`.
- **Windows install mode** is `passive` — the user sees a brief
  installer GUI, no clicks needed. Switch to `quiet` for fully
  silent (less obvious to the user) or `basicUi` for the standard
  NSIS dialog.
- **Code signing** (the OS-level kind, separate from minisign) is
  not yet configured. On Windows, that means SmartScreen will
  warn on first install. On macOS, Gatekeeper will block unsigned
  apps. Plan to set up Apple Developer + Windows EV cert before a
  public 1.0 release.

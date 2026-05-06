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

The CI workflow at `.github/workflows/release.yml` does the build,
sign, and upload steps automatically when a `v*` tag is pushed (or
when re-run manually via `workflow_dispatch` with an existing tag).

### Required repository secrets

Set these once per repository (Settings → Secrets and variables →
Actions):

| Secret | What it is |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | raw contents of `~/.tauri/waveflow.key` |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | passphrase for the above key |
| `SIGNTOOL_PFX_BASE64` | `base64 -w0 < cert.pfx` for Windows Authenticode |
| `SIGNTOOL_PFX_PASSWORD` | PFX export passphrase |

### 1. Bump the version

Edit `src-tauri/tauri.conf.json` → `version` and `package.json` →
`version` (keep them in sync). Use semver. Commit and tag:

```sh
git commit -am "chore: release v0.2.0"
git tag v0.2.0
git push origin main v0.2.0
```

### 2. Watch the workflow

Pushing the tag triggers `.github/workflows/release.yml`:

- Builds Linux (`AppImage` + `.tar.gz` + `.sig`) on `ubuntu-latest`
- Builds Windows (`*-setup.exe` + `.sig`) on `windows-latest`, signs
  the installer with the Authenticode PFX
- Builds macOS (`*.dmg` + `*.app.tar.gz` + `.sig`) on `macos-latest`
  as a universal binary covering both Intel and Apple Silicon. The
  bundle is **not** Apple-Developer-signed (no cert configured), so
  Gatekeeper will warn first-launch users — they have to right-click
  → Open once. The minisign signature on the updater payload is
  still produced normally, so auto-updates work.
- Generates a per-platform `latest-<platform>.json`
- Creates the GitHub release if missing (with auto-generated notes
  from the commit log) and uploads every artefact
- A follow-up job merges the per-platform manifests into a single
  `latest.json` and uploads that too

The Tauri updater plugin reads
`https://github.com/<owner>/<repo>/releases/latest/download/latest.json`
on app launch — that URL resolves to the merged manifest the workflow
just published.

### 3. (Optional) Re-run a release

If a build fails partway and you want to retry without re-tagging,
use the **Run workflow** button on the Release workflow page and
pass the existing tag (e.g. `v0.2.0`) as input.

### 4. Verify

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
- **Windows Authenticode signing** is wired through the release
  workflow via `SIGNTOOL_PFX_BASE64` + `SIGNTOOL_PFX_PASSWORD`
  secrets. SmartScreen still warns on first install with a fresh
  cert until enough downloads accumulate reputation; an EV cert
  shortcuts that. **macOS code signing** (Apple Developer cert) is
  not configured — the macOS job ships an unsigned universal binary
  that triggers Gatekeeper on first launch. Users right-click the
  app and pick "Open" once to allow it. To remove that friction,
  add Apple Developer ID + notarization in the macOS job (set
  `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_ID`,
  `APPLE_TEAM_ID`, and `APPLE_PASSWORD` secrets and pass them to
  `tauri build`).

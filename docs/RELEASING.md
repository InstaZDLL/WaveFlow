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

| Secret                               | What it is                                                                                                                                                                     |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `TAURI_SIGNING_PRIVATE_KEY`          | raw contents of `~/.tauri/waveflow.key`                                                                                                                                        |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | passphrase for the above key                                                                                                                                                   |
| `SIGNTOOL_PFX_BASE64`                | `base64 -w0 < cert.pfx` for Windows Authenticode                                                                                                                               |
| `SIGNTOOL_PFX_PASSWORD`              | PFX export passphrase                                                                                                                                                          |
| `AUR_SSH_PRIVATE_KEY`                | private half of the SSH key registered on the maintainer's AUR account, used by `.github/workflows/aur.yml` to push PKGBUILD updates                                           |
| `WINGET_PAT`                         | GitHub Personal Access Token (classic, `public_repo` scope) the `.github/workflows/winget.yml` action uses to fork microsoft/winget-pkgs and open the PR with the new manifest |
| `COPR_LOGIN`                         | `login` field from <https://copr.fedorainfracloud.org/api/> — `.github/workflows/copr.yml` uses it to authenticate to Fedora COPR via `copr-cli`                               |
| `COPR_TOKEN`                         | `token` field from the same COPR API page (paired with `COPR_LOGIN`). Token lifetime is 6 months — rotate when builds start returning `401 Unauthorized`                       |
| `BUILDKITE_PACKAGES_TOKEN`           | Buildkite API token (`read_packages` + `write_packages`) for `.github/workflows/apt-publish.yml` to push the `.deb` to the `instazdll/waveflow` registry                       |

The AUR package itself (`waveflow-bin`) needs a one-off manual setup
on the maintainer's box — see [`packaging/aur/README.md`](../packaging/aur/README.md).

The maintainer keeps local copies of all four key/cert files under
`secrets/` (gitignored — see [`.gitignore`](../.gitignore)) so they
can be re-uploaded when rotating. Push them to GitHub Actions secrets
with the `gh` CLI:

```powershell
# Linux/macOS shell users: drop the "Get-Content -Raw" wrapper and pipe
#                           the file directly to gh secret set.
Get-Content -Raw secrets/aur | gh secret set AUR_SSH_PRIVATE_KEY
Get-Content -Raw secrets/waveflow.key | gh secret set TAURI_SIGNING_PRIVATE_KEY
[Convert]::ToBase64String([IO.File]::ReadAllBytes((Resolve-Path secrets/cert.pfx))) | gh secret set SIGNTOOL_PFX_BASE64
gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --body "<passphrase>"
gh secret set SIGNTOOL_PFX_PASSWORD            --body "<passphrase>"
```

### 1. Cut the release via release-please

You do **not** hand-bump versions anymore. Every push to `main`
runs [`.github/workflows/release-please.yml`](../.github/workflows/release-please.yml),
which:

1. Parses the Conventional Commits since the last tag.
2. Computes the next semver (`feat:` → minor, `fix:` → patch,
   `feat!:` / `BREAKING CHANGE:` → major, no relevant commits → no PR).
3. Opens or refreshes a **chore(main): release X.Y.Z** PR that
   bumps every version manifest in lockstep:
   - [`package.json`](../package.json) (canonical, owned by release-please)
   - [`src-tauri/tauri.conf.json`](../src-tauri/tauri.conf.json) (`$.version`)
   - [`src-tauri/Cargo.toml`](../src-tauri/Cargo.toml) (`$.package.version`)
   - [`README.md`](../README.md) version badge (`x-release-please-version` annotation)
   - [`CHANGELOG.md`](../CHANGELOG.md) (auto-generated entry)
4. A second workflow
   ([`release-please-bump-lockfile.yml`](../.github/workflows/release-please-bump-lockfile.yml))
   runs `cargo check` on the PR branch and amends `src-tauri/Cargo.lock`
   so it stays consistent with the bumped `Cargo.toml`.

To ship, **review and merge the release PR** (squash or merge,
release-please tolerates both). The merge causes release-please to:

- create the `vX.Y.Z` tag on `main`,
- create the matching GitHub Release with the auto-generated notes,
- update [`.release-please-manifest.json`](../.release-please-manifest.json)
  to record the new version.

The new tag triggers [`release.yml`](../.github/workflows/release.yml)
exactly as before — no change needed downstream.

### 2. Watch the workflow

Pushing the tag triggers `.github/workflows/release.yml`:

- Builds Linux on `ubuntu-latest` — produces an `.AppImage`
  (universal, also the updater payload), a `.deb` (Debian/Ubuntu),
  and an `.rpm` (Fedora/RHEL).
- Builds Windows on `windows-latest`, signs every artefact with
  the Authenticode PFX — produces a `*-setup.exe` (NSIS, per-user
  under `%LOCALAPPDATA%`, also the updater payload) and a `.msi`
  (system-wide install for IT deployment).
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
- A separate workflow (`aur.yml`) reacts to the `release.published`
  event, bumps `packaging/aur/PKGBUILD`, refreshes `sha256sums` /
  `.SRCINFO`, and pushes the result to
  `ssh://aur@aur.archlinux.org/waveflow-bin.git` so Arch users get
  the new version through `yay`/`paru` automatically

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

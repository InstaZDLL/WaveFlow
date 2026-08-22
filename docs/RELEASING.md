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
placeholder in [`src-tauri/crates/app/tauri.conf.json`](../src-tauri/crates/app/tauri.conf.json):

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

### Optional: macOS code signing

The macOS job signs the bundle either way. Without the secrets below it
falls back to an **ad-hoc** signature, which is enough to give the app a
sealed bundle and the stable `app.waveflow` identifier — see
[macOS signing and the TCC prompt](#macos-signing-and-the-tcc-prompt) for
why that matters — but not enough for Gatekeeper. Setting them switches
the job to a real Developer ID signature plus notarization.

| Secret                       | What it is                                                                                                             |
| ---------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `APPLE_CERTIFICATE`          | `base64 -i cert.p12` of the **Developer ID Application** certificate exported from Keychain Access (private key included) |
| `APPLE_CERTIFICATE_PASSWORD` | passphrase used at `.p12` export time                                                                                  |
| `APPLE_SIGNING_IDENTITY`     | full identity string, e.g. `Developer ID Application: Jane Doe (A1B2C3D4E5)` — read it from `security find-identity -v -p codesigning` |
| `APPLE_ID`                   | Apple ID email of the developer account                                                                                |
| `APPLE_PASSWORD`             | **app-specific** password generated at <https://appleid.apple.com> — not the account password                           |
| `APPLE_TEAM_ID`              | 10-character team identifier from <https://developer.apple.com/account>                                                |

The first three are what enables signing; the last three add
notarization on top. Providing only part of either group fails the build
on purpose, rather than silently shipping a half-signed bundle.

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
   - [`src-tauri/crates/app/tauri.conf.json`](../src-tauri/crates/app/tauri.conf.json) (`$.version`)
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
  as a universal binary covering both Intel and Apple Silicon, and
  codesigns the bundle — with a Developer ID + notarization when the
  `APPLE_*` secrets are set, ad-hoc otherwise. Ad-hoc still leaves
  Gatekeeper warning first-launch users (right-click → Open once).
  The minisign signature on the updater payload is produced
  independently, so auto-updates work either way.
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

## Beta channel

WaveFlow ships an **opt-in beta channel** so testers can run
pre-release builds without affecting the stable population. Users
enable it under **Settings → Diagnostics → Beta channel**; the choice
persists in `app_setting['updater.channel']` (app-wide).

### How isolation works

The in-app updater is driven from Rust
([`commands/updater.rs`](../src-tauri/crates/app/src/commands/updater.rs))
rather than the JS `check()` so it can pick the endpoint matching the
active channel at runtime (the JS API only reads the static config
endpoints):

- **stable** → `releases/latest/download/latest.json`. GitHub's
  `/releases/latest` alias **excludes pre-releases**, so a stable user
  is structurally incapable of seeing a beta — no flag, no opt-out
  needed.
- **beta** → `releases/download/beta-channel/latest-beta.json`. The
  `beta-channel` release is a **fixed, rolling** release whose
  `latest-beta.json` asset `release.yml` re-uploads (`--clobber`) on
  every pre-release tag. A pinned tag is required because GitHub has no
  "latest pre-release" URL alias.

Toggling **off** returns the user to stable on the next check: a
released `1.5.2` is semver-greater than their `1.5.2-beta.N`, so the
stable endpoint serves it as an update.

### Cutting a beta

Run the **Cut beta** workflow
([`cut-beta.yml`](../.github/workflows/cut-beta.yml)) from the Actions
tab with a `base_version` input (e.g. `1.5.2`). It computes the next
`v1.5.2-beta.N` tag, pushes it, and dispatches `release.yml` for it.
release-please is untouched — it still owns stable versions only; betas
live entirely outside its flow.

When the tag carries a semver pre-release suffix (`-beta`, `-rc`, …),
`release.yml`:

- marks the GitHub release `--prerelease`;
- publishes the merged manifest as `latest-beta.json` on the
  `beta-channel` release (instead of `latest.json` on the version
  release);
- **skips** the downstream AUR / Winget / COPR / apt dispatch — those
  are stable repositories only.

The per-platform installers still land on the `v1.5.2-beta.N`
pre-release; the beta manifest's payload URLs point there.

> **Tagging note:** cutting a beta is the one sanctioned exception to
> the "never hand-tag" rule — but it's still automated (the workflow
> creates the tag, you don't). The rule's intent (don't bypass
> release-please for _stable_ versions) is preserved.

## The AppImage is post-processed, and it has to stay that way

Tauri's AppImage bundler copies the webview's dependency closure into the AppDir, and that closure contains `libwayland-client.so.0` — a library the [official AppImage excludelist](https://github.com/AppImage/pkg2appimage/blob/master/excludelist) explicitly forbids bundling, because [Mesa breaks against a bundled copy](https://gitlab.freedesktop.org/mesa/mesa/-/issues/11316).

We build on Ubuntu. On any host with a newer Mesa — Fedora 44 ships Mesa 26 and libwayland 1.25 — the bundled copy wins the lookup and WebKit's WebProcess dies before painting anything:

```text
Could not create default EGL display: EGL_BAD_PARAMETER. Aborting...
```

The Rust side is unaffected: the app starts, creates its profile, opens its audio device, and shows an empty window. That asymmetry is why this was misread for months as a WebKitGTK 2.52 incompatibility, with users told to install a native package instead. It is not a WebKit problem, and it is not unfixable — [`scripts/fix-appimage.sh`](../scripts/fix-appimage.sh) removes the one file, repacks the squashfs with the compressor the bundler used, verifies the result re-extracts, and re-signs when an updater `.sig` is present.

Both `release.yml` and `test-appimage.yml` run it right after `tauri build`. **If you ever restructure the Linux build, that step has to survive** — dropping it silently produces a release that installs fine and never opens, on exactly the distributions least likely to be in your test matrix. The script no-ops (and says so) if a future Tauri stops bundling the library, so it's safe to leave in place.

## Flatpak sources are generated, and they go stale silently

Flathub builds with the network disabled, so **every crate and npm tarball must be pre-declared** in [`packaging/flatpak/generated/`](../packaging/flatpak/generated/) by [`generate-sources.sh`](../packaging/flatpak/generate-sources.sh).

A `Cargo.lock` / `package.json` bump that skips that regeneration does **not** fail CI — it fails much later, inside Flathub's sandbox, on a crate nobody can download. `chore: update dependencies` (23be643) left **53** crates undeclared exactly this way.

[`check-sources.py`](../packaging/flatpak/check-sources.py) guards the invariant on every PR touching a lockfile. It verifies **coverage** — every registry crate in `Cargo.lock` has a source entry, every npm package in the Flatpak lockfile has a tarball — offline, in about a second.

It deliberately does **not** regenerate-and-diff: the generator runs `npm install --package-lock-only`, which re-resolves `^` ranges against the live registry, so such a check would go red whenever any transitive dependency publishes. Freshness is instead a monthly chore ([`flatpak-sources-refresh.yml`](../.github/workflows/flatpak-sources-refresh.yml), also `workflow_dispatch`-able) that regenerates and opens a PR.

> **Toolchain split.** The app builds from `bun.lock`, but the Flatpak sandbox runs `npm ci --offline` against a **separate npm lockfile** (`flatpak-node-generator` can't read `bun.lock`). A dependency pin that must hold for shipped builds therefore belongs in `package.json` — e.g. an `overrides` entry — not in `bun.lock` alone.

## macOS signing and the TCC prompt

Signing the macOS bundle is not only about Gatekeeper. It is what stops macOS from asking for folder permission **on every single launch**.

macOS records a TCC grant — the "WaveFlow would like to access files in your Downloads folder" consent — against the app's *designated requirement*, i.e. its code-signing identity. Before this was wired up, the shipped bundle carried only the ad-hoc signature the linker adds automatically on Apple Silicon:

```
Identifier=waveflow-36c95c4d36c8f6a6      ← generated, not app.waveflow
Signature=adhoc
flags=0x20002(adhoc,linker-signed)
Info.plist=not bound
Sealed Resources=none
```

There is no `Contents/_CodeSignature` in that state: the `.app` was never passed to `codesign`, only its Mach-O binary was. With no sealed bundle and no stable identifier, there is nothing to anchor the grant to, so it never persists. Any user whose library lives under `~/Downloads`, `~/Desktop`, `~/Documents`, an external drive or a network share re-consents at every launch.

Signing the bundle fixes it, and **ad-hoc is enough for this specific problem** — `codesign -s -` still produces sealed resources, a bound `Info.plist` and the `app.waveflow` identifier from the bundle. What ad-hoc does *not* do is survive an update (each build is a different identity, so the grant resets on upgrade) or satisfy Gatekeeper. A Developer ID identity is stable across versions and does both.

Three pieces make this work, and they have to stay together:

- [`Info.plist`](../src-tauri/crates/app/Info.plist) — the `NS*UsageDescription` purpose strings macOS shows in the consent dialog. Auto-merged by the bundler because it sits next to `tauri.conf.json`.
- [`waveflow.entitlements`](../src-tauri/crates/app/waveflow.entitlements) — hardened runtime is on by default in the bundler and mandatory for notarization, so `com.apple.security.cs.allow-jit` has to be declared or the wasmtime plugin host crashes on the first plugin load.
- The `Resolve macOS signing mode` + `Verify macOS signature` steps in [`release.yml`](../.github/workflows/release.yml).

Signing happens **inside** `tauri build`, driven by `APPLE_SIGNING_IDENTITY`, not as a post-build `codesign` pass. That ordering is not cosmetic: the bundler builds the DMG and the `.app.tar.gz` updater payload *from* the `.app`, so signing afterwards would leave both wrapping an unsigned copy.

The verify step asserts the sealed-resources / `app.waveflow` / designated-requirement triple, because a silent signing failure would look like a clean release and quietly bring the per-launch prompt back.

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
  shortcuts that.

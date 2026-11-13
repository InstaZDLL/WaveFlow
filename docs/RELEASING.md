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

### 3. Confirm the endpoints

The updater reads two hosts, GitHub Releases and its mirror on
Cloudflare R2:

```
https://github.com/InstaZDLL/WaveFlow/releases/latest/download/latest.json
https://download.waveflow.app/stable/latest.json
```

They are compiled into the binary — in
[`commands/updater.rs`](../src-tauri/crates/app/src/commands/updater.rs),
which drives every check, and repeated in `tauri.conf.json`, which a
test keeps equal to it. A host added later reaches only the builds
released after it, which is why the mirror shipped before anything
needed it: see
[Second download source](#second-download-source-cloudflare-r2).

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
| `SIGNPATH_API_TOKEN`                 | API token of the SignPath CI user, for the Windows signing ([SignPath](#windows-signing-with-signpath))                                                                        |
| `AUR_SSH_PRIVATE_KEY`                | private half of the SSH key registered on the maintainer's AUR account, used by `.github/workflows/aur.yml` to push PKGBUILD updates                                           |
| `WINGET_PAT`                         | GitHub Personal Access Token (classic, `public_repo` scope) the `.github/workflows/winget.yml` action uses to fork microsoft/winget-pkgs and open the PR with the new manifest |
| `COPR_LOGIN`                         | `login` field from <https://copr.fedorainfracloud.org/api/> — `.github/workflows/copr.yml` uses it to authenticate to Fedora COPR via `copr-cli`                               |
| `COPR_TOKEN`                         | `token` field from the same COPR API page (paired with `COPR_LOGIN`). Token lifetime is 6 months — rotate when builds start returning `401 Unauthorized`                       |
| `APT_GPG_PRIVATE_KEY`                | armored secret key that signs the APT repository ([APT repository](#apt-repository)); `.github/workflows/apt-publish.yml` imports it for the run only                         |
| `APT_GPG_PASSPHRASE`                 | passphrase of that key                                                                                                                                                         |

### Optional: the R2 mirror

The four secrets that let the `Mirror to R2` job publish the
[second download source](#second-download-source-cloudflare-r2). With
none of them the job ends green after one step and the release is not
mirrored; with only some of them it fails, alone — the GitHub release,
its manifest and the downstream dispatches are already out by then.

| Secret                 | What it is                                                                                                                                                                                                                                |
| ---------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `R2_ACCESS_KEY_ID`     | Access Key ID of an R2 API token with **Object Read & Write** on the bucket alone                                                                                                                                                         |
| `R2_SECRET_ACCESS_KEY` | its Secret Access Key, shown once when the token is created                                                                                                                                                                               |
| `R2_ACCOUNT_ID`        | the Cloudflare account id (R2 overview page), 32 hex characters; the S3 endpoint shown beside it, `https://<id>.r2.cloudflarestorage.com`, is read as the same id, and anything else stops the mirror job with an error naming the secret |
| `R2_BUCKET`            | the bucket's name                                                                                                                                                                                                                         |

The bucket's public domain is a repository **variable**, `R2_PUBLIC_URL`,
needed only if it is not `https://download.waveflow.app` — and then the
constants in `commands/updater.rs` have to change with it, in a release
that ships before the domain is relied on.

### Optional: macOS code signing

The macOS job signs the bundle either way. Without the secrets below it
falls back to an **ad-hoc** signature, which is enough to give the app a
sealed bundle and the stable `app.waveflow` identifier — see
[macOS signing and the TCC prompt](#macos-signing-and-the-tcc-prompt) for
why that matters — but not enough for Gatekeeper. Setting them switches
the job to a real Developer ID signature plus notarization.

| Secret                       | What it is                                                                                                                             |
| ---------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| `APPLE_CERTIFICATE`          | `base64 -i cert.p12` of the **Developer ID Application** certificate exported from Keychain Access (private key included)              |
| `APPLE_CERTIFICATE_PASSWORD` | passphrase used at `.p12` export time                                                                                                  |
| `APPLE_SIGNING_IDENTITY`     | full identity string, e.g. `Developer ID Application: Jane Doe (A1B2C3D4E5)` — read it from `security find-identity -v -p codesigning` |
| `APPLE_ID`                   | Apple ID email of the developer account                                                                                                |
| `APPLE_PASSWORD`             | **app-specific** password generated at <https://appleid.apple.com> — not the account password                                          |
| `APPLE_TEAM_ID`              | 10-character team identifier from <https://developer.apple.com/account>                                                                |

The first three are what enables signing; the last three add
notarization on top. Providing only part of either group fails the build
on purpose, rather than silently shipping a half-signed bundle — and so
does the cross-group case, notarization credentials without a
certificate, since there would be nothing signed to submit.

Setting **none** of the six stays a supported configuration: that is the
ad-hoc build described above, chosen deliberately rather than reached by
accident.

The AUR package itself (`waveflow-bin`) needs a one-off manual setup
on the maintainer's box — see [`packaging/aur/README.md`](../packaging/aur/README.md).

The maintainer keeps local copies of the key files under
`secrets/` (gitignored — see [`.gitignore`](../.gitignore)) so they
can be re-uploaded when rotating. Push them to GitHub Actions secrets
with the `gh` CLI:

```powershell
# Linux/macOS shell users: drop the "Get-Content -Raw" wrapper and pipe
#                           the file directly to gh secret set.
Get-Content -Raw secrets/aur | gh secret set AUR_SSH_PRIVATE_KEY
Get-Content -Raw secrets/waveflow.key | gh secret set TAURI_SIGNING_PRIVATE_KEY
gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --body "<passphrase>"
# the SignPath CI user's token, pasted when gh asks for it:
gh secret set SIGNPATH_API_TOKEN
```

### 1. Cut the release via release-please

A version with something to tell also gets its [What's new pages](features/ui.md#adding-the-pages-of-a-version), written by hand before the cut; a correction release needs none.

You do **not** hand-bump versions anymore. Every push to `main` — or to
a maintenance branch, see
[Patch releases from a maintenance branch](#patch-releases-from-a-maintenance-branch) —
runs [`.github/workflows/release-please.yml`](../.github/workflows/release-please.yml)
against that branch, which:

1. Parses the Conventional Commits since the last tag.
2. Computes the next semver (`feat:` → minor, `fix:` → patch,
   `feat!:` / `BREAKING CHANGE:` → major, no relevant commits → no PR).
3. Opens or refreshes a release PR — **chore(main): release X.Y.Z** on
   `main`, **chore(release/X.Y.x): release X.Y.Z+1** on a maintenance
   branch — that
   bumps every version manifest in lockstep:
   - [`package.json`](../package.json) (canonical, owned by release-please)
   - [`src-tauri/crates/app/tauri.conf.json`](../src-tauri/crates/app/tauri.conf.json) (`$.version`)
   - [`src-tauri/Cargo.toml`](../src-tauri/Cargo.toml) (`$.package.version`)
   - [`CHANGELOG.md`](../CHANGELOG.md) (auto-generated entry)
4. A second workflow
   ([`release-please-bump-lockfile.yml`](../.github/workflows/release-please-bump-lockfile.yml))
   runs `cargo check` on the PR branch and amends `src-tauri/Cargo.lock`
   so it stays consistent with the bumped `Cargo.toml`.

To ship, **review and merge the release PR** (squash or merge,
release-please tolerates both). The merge causes release-please to:

- create the `vX.Y.Z` tag on the branch it targets (`main`, or the
  maintenance branch),
- create the matching GitHub Release with the auto-generated notes,
- update [`.release-please-manifest.json`](../.release-please-manifest.json)
  to record the new version.

The tag is created with the workflow's `GITHUB_TOKEN`, and a tag pushed
that way **does not trigger** [`release.yml`](../.github/workflows/release.yml)
(GitHub does not start workflows from events the token itself caused).
Start it by hand once the tag exists:

```bash
gh workflow run release.yml -f tag_name=vX.Y.Z
```

The workflow tags first and updates the release PR second: in the other
order, the update right after a release PR merged went looking for a
release that did not exist yet, walked up to 1,500 commits one API call at
a time, and the job's timeout cut it before the tag was ever made.

### 2. Watch the workflow

`.github/workflows/release.yml`, started for the tag as above (a tag
pushed by hand would start it on its own; release-please's does not):

- Builds Linux on `ubuntu-24.04` — produces an `.AppImage`
  (universal, also the updater payload), a `.deb` (Debian/Ubuntu),
  and an `.rpm` (Fedora/RHEL); a pre-release has no `.rpm`, since rpm
  refuses a `-` in its version. The runner is pinned, never
  `ubuntu-latest`: a binary needs at least the glibc it was linked
  against, so the runner's glibc is the oldest one WaveFlow starts
  on. When `ubuntu-latest` moved to 26.04, the 1.9.6 binaries
  needed glibc 2.43 and stopped starting on Ubuntu 24.04, Debian 13 and
  Mint. A step after the AppImage is final fails the release if the
  binary, or any library the AppImage bundles from the runner, needs a
  glibc newer than `GLIBC_FLOOR` (2.39); raise both together, on
  purpose — and `.gitlab-ci.yml`'s `ubuntu:24.04` image and its own
  `GLIBC_FLOOR` with them.
- Builds Windows on `windows-latest` and signs it through SignPath in
  two requests, each waiting for a manual approval
  ([below](#windows-signing-with-signpath)) — produces a `*-setup.exe`
  (NSIS, per-user under `%LOCALAPPDATA%`, also the updater payload) and
  a `.msi` (system-wide install for IT deployment). The Windows job may
  therefore run for hours; its limit is 360 minutes, the most GitHub
  allows a hosted job.
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
- A last job, `Mirror to R2`, copies the release's files onto the R2
  bucket with a manifest pointing at them — see
  [Second download source](#second-download-source-cloudflare-r2)
- A separate workflow (`aur.yml`) reacts to the `release.published`
  event, bumps `packaging/aur/PKGBUILD`, refreshes `sha256sums` /
  `.SRCINFO`, and pushes the result to
  `ssh://aur@aur.archlinux.org/waveflow-bin.git` so Arch users get
  the new version through `yay`/`paru` automatically

The Tauri updater plugin reads
`https://github.com/<owner>/<repo>/releases/latest/download/latest.json`
on app launch — that URL resolves to the merged manifest the workflow
just published — and its R2 mirror, `stable/latest.json`.

The code that is not GitHub glue lives in
[`scripts/release/`](../scripts/release/): the asset names and the
per-platform manifest (`prepare-assets.py`), the merge
(`merge-manifests.py`), the glibc floor check, the R2 upload, and the
version comparison (`version-newer.py`, which exits 2 rather than "not
newer" when it is handed something that is not a version). It keeps
an R2 channel from moving backwards, unless `R2_FORCE=1`, and decides
on GitHub whether a stable release also goes to the beta channel. A
pre-release itself replaces `latest-beta.json` on GitHub without that
comparison, as it always has, so re-running an older beta's tag moves
the GitHub beta channel back while its R2 copy stays put. The
workflow runs them from its own commit, checked out next to the tag
(`.release-tools/`), so a patch release whose tag predates them builds
the same way; [`.gitlab-ci.yml`](../.gitlab-ci.yml) runs the same files.

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

## Patch releases from a maintenance branch

Once `main` carries features for the next minor or major, a fix for the
version users have cannot be cut from it: release-please would bump to
the next minor and ship those features with it. A patch is cut from a
maintenance branch instead, `release/X.Y.x`, branched from the last tag
of that line:

1. **Create the branch** from the tag: `git push origin vX.Y.Z:refs/heads/release/X.Y.x`.
2. **Bring the workflows** to it. A push runs the pushed branch's copy of
   [`release-please.yml`](../.github/workflows/release-please.yml), and
   a pull request runs its base's copy of
   [`release-please-lockfile-build.yml`](../.github/workflows/release-please-lockfile-build.yml);
   a tag older than their `release/**` support has neither. Cherry-pick
   the commit that added it.
3. **Cherry-pick each fix** from `main` (signed: `git cherry-pick -S`),
   through a pull request into `release/X.Y.x` so CI runs on it. The
   **Maintenance branches** ruleset protects `release/**` the way `Main`
   protects `main` — no deletion or force push, signed commits, a pull
   request with one approval — without the CodeQL gate, since CodeQL only
   analyses pull requests into `main`. A push to these branches runs their
   own copy of the workflow with a write token, which is why they must not
   take direct pushes.
4. release-please, run on the branch with `--target-branch`, opens
   `chore(release/X.Y.x): release X.Y.Z+1` against it; the Cargo.lock
   pipeline fills it in as on `main`. Merge it, then start `release.yml`
   for the new tag as above. Downstream (AUR, winget, COPR, apt) and the
   updater manifest work the same, since everything starts from the tag.

`main` keeps its own manifest, so its next release still counts from its
last stable version; its changelog will list the cherry-picked fixes a
second time, under the version that ships them on `main`. For a major,
set `release-as` in [`release-please-config.json`](../release-please-config.json)
before merging the release PR.

## Beta channel

WaveFlow ships an **opt-in beta channel** so testers can run
pre-release builds without affecting the stable population. Users
enable it under **Settings → Maintenance → Updates → Beta channel**; the choice
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

The check at launch and the one after a channel switch are automatic:
they read no manifest while **Settings → Maintenance → Updates → Check
for updates automatically** is off (`app_setting['updater.auto_check']`,
on by default) or offline mode is on. **Check now** on the same card
still reads them with the switch off
([`ui.md`](features/ui.md#auto-updater)).

**Staying on the beta channel follows stable releases too** (#793). Until 1.8.0 a tester read `latest-beta.json` alone, which only a pre-release tag ever rewrote, so someone on `1.8.0-beta.3` never saw `1.8.0`. Two changes close that, one per side:

- the updater reads **both** manifests on the beta channel and keeps the newer answer ([`updater.rs`](../src-tauri/crates/app/src/commands/updater.rs), `is_newer`, by semver). One manifest failing does not hide what the other found;
- `release.yml` also copies a stable manifest onto `beta-channel` when that stable is newer than the beta published there. That is what reaches builds that predate the first change. A stable that is older — a `1.8.1` fix while `1.9.0-beta.1` is out — leaves the channel alone.

For 1.8.0 itself, the stable manifest was uploaded onto `beta-channel` by hand on 2026-10-02.

From 2.0 each of the two manifests has an R2 mirror (`beta/latest.json`, `stable/latest.json`), and the beta channel reads all four. The stable-onto-beta copy has no mirror twin: a build that reads R2 reads `stable/latest.json` on the beta channel anyway.

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

## Second download source (Cloudflare R2)

Every installed WaveFlow finds its updates at the addresses compiled into it. Until 2.0 that was one GitHub URL, so the day the repository or the account stopped answering, every installation would have stopped updating for good, with no release able to tell it otherwise. From 2.0 each manifest has a mirror on an R2 bucket served under the project's own domain, filled by `release.yml` with the same files:

```text
https://download.waveflow.app/
├── releases/<tag>/…           every asset of the release, same names, same bytes
├── stable/latest.json         the manifest of the last stable tag
├── beta/latest.json           the manifest of the last pre-release tag
└── plugins/                   the plugin store's copy (plugins-mirror.yml, below)
    ├── registry.json
    └── <id>/<id>-v<version>.zip
```

The mirrored manifest is the merged `latest.json` with each `url` pointing at the copy under `releases/<tag>/` ([`mirror-manifest.py`](../scripts/release/mirror-manifest.py)) and nothing else changed. The signatures are the ones the build made: minisign signs the payload's bytes, not the address it came from, so the app verifies an update from either host under its one public key. The script refuses to write a manifest naming a file it is not about to upload, or a signature that is not the one in the `.sig` next to it.

### How the app reads two hosts

Each channel reads the GitHub manifest and its mirror ([`UpdateChannel::endpoints`](../src-tauri/crates/app/src/commands/updater.rs)) — four manifests on the beta channel, two on stable — one request at a time, and keeps the newest version any of them offers. One host failing, whatever the way, hides nothing the other found; only all of them failing is an error. Each manifest gets 30 seconds to answer: the plugin sets no timeout, and a host that accepts the connection and never replies would otherwise keep the next one from being asked. That limit is on the check alone: the download that follows may take as long as it needs, but not 30 seconds without a byte. The update then downloads from the host whose manifest offered it. When both offer the same version, both are kept (`install_order`), GitHub's first: if its payload cannot be downloaded, falls silent or does not verify — a host that answers the small manifest but not the 100 MB file — the other host's copy is tried. An older version is never a fallback, and once an installer has started its error is final.

They are deliberately **not** handed to `tauri-plugin-updater` as one list. Its own loop (read in 2.14.1, `Updater::check`):

| The host answers                    | The plugin                                                         |
| ----------------------------------- | ------------------------------------------------------------------ |
| a network error                     | remembers the error, tries the next endpoint                       |
| a status other than 2xx (404, 5xx)  | tries the next endpoint, without recording an error                |
| 204                                 | stops: "no update", the next endpoint is never asked               |
| 200 with a body that is not JSON    | **stops with that error**, the next endpoint is never asked        |
| JSON that is not a release manifest | remembers the error, tries the next endpoint                       |
| a valid manifest                    | stops there, without comparing its version to any other endpoint's |

A suspended account's page or a captive portal is the fourth row, and the last row means a stale first host would hide a newer second one. Reading them one by one in our own loop avoids both. The `tauri.conf.json` list is not read by anything — every check goes through the Rust command — and names the stable pair only so the two cannot disagree silently (`the_static_config_names_the_stable_pair`).

**Keeping the newest answer is safe only because the version is signed.** A manifest is not signed, only the payload is: without more, either host — or anyone able to serve its address — could announce `99.0.0` over the URL and signature of an older genuine build, and since the newest answer wins, walk every installation back to it. `requireSignedVersion` is on in `tauri.conf.json` (a test keeps it on): the updater then refuses a payload whose signature's trusted comment does not name the version the manifest announces. `tauri build` writes that version into the signature by itself; **anything that re-signs a payload afterwards has to carry it over** — `fix-appimage.sh` and `appimage-update-info.sh` read it from the signature they replace and pass it to `signer sign --app-version`, and stop if it is missing. Until 1.9.8 they did not, so the Linux signatures of those releases name no version; that matters to no 2.0 installation, which is only ever offered newer releases.

Until the bucket and its domain exist, the mirror requests fail on DNS and each check logs one warning per mirror; the GitHub answers are unaffected.

### How a release reaches it

The `Mirror to R2` job of `release.yml` runs last, after the GitHub release and its manifest are out, and calls [`publish-r2.sh`](../scripts/release/publish-r2.sh):

1. downloads the release's assets from GitHub and the merged manifest from the run;
2. uploads the assets under `releases/<tag>/` through R2's S3 API;
3. asks the public domain for each payload the manifest names, and stops unless it answers with the local file's size — the S3 API accepting a file says nothing about the domain serving it;
4. leaves the channel's manifest alone when the one already published is newer — a patch release from a maintenance branch, or a re-run on an old tag, would otherwise move every installation's view of this host backwards; the same version again is a re-run and goes through, and `R2_FORCE=1` overrides for a deliberate rollback;
5. uploads the manifest (`no-cache`) last, so it never points at a file that is not there yet, and reads it back through the domain.

The payloads are cached five minutes at the edge, not a year: re-running the release workflow rebuilds them under the same names, and a stale copy would no longer match the new manifest's signature.

A tag's files keep their address: `releases/<tag>/` is rewritten in place if a tag is ever published again. Do not rebuild a tag with different bytes. Should it happen, an installation that read the old manifest, or a cache that kept the old file, gets bytes whose signature does not match: the install stops with an error, the updater tries the other host, and nothing wrong is installed. Publishing a new version is the way out.

One mirror runs at a time per channel on GitHub (the `r2-mirror-stable` and `r2-mirror-beta` concurrency groups). GitHub keeps one pending run per group and cancels the one that was waiting, by order of arrival, not by version: when two releases of one channel reach the mirror together and the older arrives last, the newer one's mirror is cancelled. Release one version per channel at a time, and if that happens, re-run the cancelled Mirror to R2 job. GitLab runs one publish at a time (the `r2-publish` resource group), so two releases cannot interleave the read and the write of a channel's manifest. These locks belong to each platform and are not shared: during a switch, stop releases on one before the other publishes to the bucket.

Nothing is ever deleted from the bucket. A release is about 400 MB across the three platforms; prune old `releases/<tag>/` folders by hand (or with a lifecycle rule on that prefix) when the storage bill says so, never the one the current manifests name.

`R2_DRY_RUN=1` runs the script without credentials or network: it rewrites the manifest, prints every upload and writes the result to `R2_DRY_RUN_OUT`.

```bash
R2_DRY_RUN=1 TAG_NAME=v2.0.0 IS_PRERELEASE=false bash scripts/release/publish-r2.sh dist-release dist-release/latest.json
```

### The plugin store mirror

The same bucket holds a copy of the plugin store, under `plugins/`. The store's registry is served by `waveflow.app` (a route of the website that merges the public `registry.json` of `InstaZDLL/waveflow-plugins` with the private plugins), and most archives live there too: without a copy, an outage of the site would empty the store of every private plugin and stop each of them from installing. The app reads the copy right after `waveflow.app`'s registry, and tries an archive's copy once when the archive's own address fails ([plugins.md](features/plugins.md#the-plugin-store)). Both addresses are compiled in.

[`plugins-mirror.yml`](../.github/workflows/plugins-mirror.yml) fills it, **every six hours** (`17 */6 * * *`) and **by hand** (Actions → Plugins mirror → Run workflow), with the same four `R2_*` secrets and the `R2_PUBLIC_URL` variable as `Mirror to R2`; none of them set ends the run green, doing nothing. It calls [`publish-plugins-r2.sh`](../scripts/release/publish-plugins-r2.sh), which shares its R2 plumbing with `publish-r2.sh` ([`r2-common.sh`](../scripts/release/r2-common.sh)):

1. [`mirror-plugins.py`](../scripts/release/mirror-plugins.py) reads the merged registry on `waveflow.app`, downloads every archive it names (its `download_url`, else its GitHub release, as the app builds that address; https only, never to an internal address, redirects included) and checks each one as the app would: the blake3 of `plugin.wasm` is the registry's, it is a component and not a core module, the manifest declares schema 1, has the required `[plugin]` fields and names the entry's id, version and world. It runs without the R2 keys in its environment, and `blake3` comes from PyPI pinned by hash ([`requirements-plugins-mirror.txt`](../scripts/release/requirements-plugins-mirror.txt));
2. the archives go up under `plugins/<id>/<id>-v<version>.zip` (five minutes at the edge, like a release's payloads);
3. each is asked for on the public domain, and must answer with the staged size;
4. `plugins/registry.json` goes up last (`no-cache`), byte for byte what `waveflow.app` served, and is read back.

**What it refuses, leaving the bucket as it was** (the run goes red, so the failure is seen):

- a registry that does not answer, is not JSON, declares a `schema_version` other than 1, or lists no plugin — an empty or unreadable copy never replaces a good one;
- an entry whose id, version or blake3 could not name a file safely, or that the app would not decode — one such entry makes the whole registry unreadable to the app, which would then skip the mirror;
- **any** archive that fails to download or to verify: nothing at all is uploaded, so the mirror's registry never names an archive the mirror does not hold or one the app would refuse;
- a registry that drops a plugin the mirror's current copy lists. An outage of the private half of the merge must not shrink the good copy, so a withdrawal is a deliberate run: start the workflow by hand with **allow removal** ticked. Until then, the mirror still lists the withdrawn plugin — only to installations that cannot reach `waveflow.app`.

Nothing is ever deleted: an archive a newer registry no longer names stays under its version. No address is logged with its query string, and the keys stay in the environment, never on a command line.

**Withdrawing a plugin** therefore takes more than the registry commit, which alone leaves the mirror listing it and serving its archive:

1. remove the entry from the registry (`InstaZDLL/waveflow-plugins`, or the private list behind `waveflow.app`);
2. run **Plugins mirror** by hand with **allow removal** ticked, so the mirror's registry drops it;
3. delete its archives from the bucket, `plugins/<id>/` (`aws s3 rm s3://<bucket>/plugins/<id>/ --recursive --endpoint-url https://<account-id>.r2.cloudflarestorage.com`, or the dashboard).

To check what a run would do without touching the bucket — the downloads and checks are real, the uploads are printed (needs `pip install blake3`):

```bash
R2_DRY_RUN=1 bash scripts/release/publish-plugins-r2.sh
```

The script's tests run in CI (`Plugins mirror script`, standard library only):

```bash
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
```

The standby [`.gitlab-ci.yml`](../.gitlab-ci.yml) has no twin of this workflow: after a switch, a scheduled pipeline running `publish-plugins-r2.sh` with the same variables does the same job.

### What the maintainer creates

Once, in the Cloudflare dashboard:

1. **An R2 bucket** (e.g. `waveflow-releases`). Public access stays off on the `r2.dev` address.
2. **A custom domain** on it, `download.waveflow.app` — the zone is already on Cloudflare. This is the address compiled into the app; changing it later strands every build that knows the old one.
3. **An R2 API token** with Object Read & Write, scoped to that bucket alone.
4. The four `R2_*` secrets in the GitHub repository ([table](#optional-the-r2-mirror)).

Then re-run the `Mirror to R2` job of the last release (or the whole workflow with its tag) to fill the bucket before 2.0 users rely on it, and run the **Plugins mirror** workflow once by hand rather than wait for its schedule.

Builds older than 2.0 know only GitHub. They cannot be moved; if GitHub goes, their users reinstall from the website once.

## APT repository

Debian and Ubuntu users install WaveFlow from a static, signed APT repository on the same R2 bucket as the updater mirror, served at `https://download.waveflow.app/apt/`. It replaces the Buildkite Packages registry, which refused uploads (HTTP 403, registry full). Pre-releases are never in it.

```sh
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSL https://download.waveflow.app/apt/waveflow.gpg -o /etc/apt/keyrings/waveflow.gpg
echo "deb [signed-by=/etc/apt/keyrings/waveflow.gpg] https://download.waveflow.app/apt stable main" | sudo tee /etc/apt/sources.list.d/waveflow.list
sudo apt update && sudo apt install wave-flow
```

The package is `wave-flow` (the name Tauri gives the Debian package), not `waveflow`.

### Structure

```
apt/waveflow.gpg                             public key, binary (for signed-by=)
apt/waveflow.asc                             the same key, armored
apt/dists/stable/{Release,Release.gpg,InRelease}
apt/dists/stable/main/binary-amd64/{Packages,Packages.gz}
apt/pool/main/w/wave-flow/wave-flow_<version>_amd64.deb
```

One suite (`stable`), one component (`main`), one architecture (`amd64`). `Release` carries Origin, Label, Suite, Codename, Architectures, Components and Date, and **no `Valid-Until`**: the repository is rebuilt on a release, not on a clock, and an expiry would make every client refuse it after a quiet month.

### How it is built

`.github/workflows/apt-publish.yml`, dispatched by `release.yml` after every stable release (and on `release: published`, stable only). It is stateless: each run downloads the `.deb` of the three newest stable releases from GitHub, renames them to the Debian pattern, and rebuilds the whole tree with `scripts/release/build-apt-repo.sh` (`apt-ftparchive` for `Packages` and `Release`, `gpg --clearsign` for `InRelease`, a detached armored signature for `Release.gpg`, then a check of both signatures against the exported key). The generator knows nothing about the destination, so the same tree can be copied to another host (a server of our own, an S3-compatible bucket) as a `mirror+` source.

`scripts/release/publish-apt-r2.sh` uploads in an order that never leaves an index naming a missing file: `pool/` (immutable, one-year cache), the keys, `Packages*`, `Release` and `Release.gpg`, `InRelease` last (`dists/` and the keys cache for a minute). Only then are the `pool/` files the new index no longer lists deleted. It refuses any prefix other than `apt` or `apt-<name>`.

The `verify` jobs then do what a user does, through the public domain, on `debian:stable`, `ubuntu:22.04` and `ubuntu:24.04`: add the repository with `signed-by`, `apt update`, `apt install wave-flow`, and compare the installed version with the tag. Any failure fails the run.

A manual run takes `tag_name` and `prefix`. Use `prefix=apt-test` to rehearse: it writes under `apt-test/` and nothing users read. A rebuild for an older tag builds the repository as it was at that tag, which is why `prefix=apt` refuses any tag but the newest stable release (it would drop the newer versions). One known limit: indexes are replaced in place, without `by-hash` paths, so a client reading during the few seconds of an upload can see a hash mismatch and succeeds on the next `apt update`.

### The signing key

- Secrets: `APT_GPG_PRIVATE_KEY` (armored secret key, RSA 4096) and `APT_GPG_PASSPHRASE`. The passphrase reaches `gpg` through a file in `$RUNNER_TEMP`, never an argument, and the keyring is deleted at the end of the job.
- Long id: `D00B159309D12EA1`. Full fingerprint: `4B92C10AD8200F3370BBAB2ED00B159309D12EA1` — pinned in the workflow (`APT_GPG_EXPECTED_FPR`); the job prints the fingerprint it found and fails if it differs, so a key replaced by mistake publishes nothing.
- **Expiry: 13 November 2031** (primary key and subkey, extended once already; the fingerprint did not change). The build prints both expiries and fails when either is under 90 days away (`APT_GPG_MIN_DAYS`), so the date shows up in a red run well before `apt update` breaks. An expired key makes `apt update` fail for every user, so extend it well before: on the machine that holds the key, `gpg --edit-key D00B159309D12EA1`, `expire`, a new duration, `save`; export it (`gpg --armor --export-secret-keys D00B159309D12EA1`) into the `APT_GPG_PRIVATE_KEY` secret, then re-run the workflow with `prefix=apt` on the latest tag. That rewrites `waveflow.gpg`, `waveflow.asc` and the signatures. The fingerprint does not change when only the expiry does. Users with the key already in `/etc/apt/keyrings/` must fetch the new `waveflow.gpg` once; the key file is the only thing that carries the new date.
- A **new** key (a lost passphrase, a leak) changes the fingerprint: update `APT_GPG_EXPECTED_FPR`, republish, and tell users to replace the keyring file.

### Moving from Buildkite

Users of `https://packages.buildkite.com/instazdll/waveflow/` must switch: remove `/etc/apt/sources.list.d/waveflow.list` and the old keyring, then follow the four commands above. The old registry is no longer updated. `BUILDKITE_PACKAGES_TOKEN` is unused and can be deleted from the repository secrets.

### On GitLab

The standby `.gitlab-ci.yml` has no APT job (it never had a Buildkite one either). Run `apt-publish.yml` from GitHub, or run `build-apt-repo.sh` and `publish-apt-r2.sh` by hand with the key in `GNUPGHOME`.

## Switching to GitLab

[`.gitlab-ci.yml`](../.gitlab-ci.yml) is a standby copy of `ci.yml`, `release.yml` and `cut-beta.yml`. Nothing runs it on GitHub. It exists so that losing the GitHub account costs a few hours of setup rather than a rewrite of the release pipeline at the worst possible moment. On GitLab, R2 is the only download host: the builds write manifests that point there directly, and the GitLab release holds links to the files rather than the files (a project's storage quota would not take an installer set per version).

### Before anything happens

The parts that need GitHub to still answer have to be done while it does:

- **The code and the tags are already there.** [`mirror-gitlab.yml`](../.github/workflows/mirror-gitlab.yml) pushes `main` and every tag to the private backup at `gitlab.com/waveflow-groupe/WaveFlow` as they land here, with a deploy key that may write to that one project (the `GITLAB_DEPLOY_KEY` secret; GitLab's pull mirroring is a paid feature). `main` and the `v*` tags are protected there with that key allowed, and the backup's CI/CD is off. The other WaveFlow repositories (server, QuickPlay, the mobile apps, the website, the plugins, Tempora) have the same job and a backup in the same group.
- **Import what the mirror does not carry** (New project → Import project → GitHub, with **Import Markdown attachments** ticked so the screenshots in issues come along; check that they did): issues, pull requests as merge requests, milestones, labels, the wiki and the releases' notes. The importer reads GitHub's API with your token, so it cannot run after the account is gone. Keep the import private and redo it now and then: each import is a new project and never updates the previous one, so note the path of the latest. That project is where the issues and merge requests are read after the switch; the backup stays the code's home.
- **Fill the R2 bucket** ([above](#what-the-maintainer-creates)), so the 2.0 builds already have a second host.
- Keep a local clone with every tag: `git fetch --tags` from GitHub is the only source of history once it is gone.

### Runners

GitLab.com's hosted runners, as of 2026-11 ([docs](https://docs.gitlab.com/ci/runners/hosted_runners/)):

| Job                            | Runner                      | Machine                     | Who can use it                                                     |
| ------------------------------ | --------------------------- | --------------------------- | ------------------------------------------------------------------ |
| checks, Linux build, publish   | `saas-linux-medium-amd64`   | 4 vCPU, 16 GB, 50 GB disk   | every tier                                                         |
| Rust on Windows, Windows build | `saas-windows-medium-amd64` | 2 vCPU, 7.5 GB, 75 GB, beta | every tier                                                         |
| macOS build                    | `saas-macos-medium-m1`      | M1, 4 vCPU, 8 GB, beta      | **Premium / Ultimate only**, or the GitLab for Open Source program |

The small default Linux runner (2 vCPU, 30 GB) is too tight for the Rust job, which peaks around 21 GB of disk before the image and toolchain. The Windows runner uses PowerShell with no container: Visual Studio Build Tools, CMake, Python and Git are preinstalled, Rust and Bun are installed by the job. Its signtool comes from the Windows Kits if the image has them; the job fails with a clear message if not.

Compute minutes are the real limit: the Free tier's 400 a month do not cover one release (three builds of 30 to 90 minutes each, macOS at a cost factor of 6). Apply to the **GitLab for Open Source program** — Ultimate features for a public, OSI-licensed project, its macOS runners included, and a 0.5 cost factor on public projects. Without it, register runners of your own: `gitlab-runner` with the shell executor on the Mac (set the `MACOS_RUNNER_TAG` variable to its tag) and, if minutes run out, on the Linux machine. A Windows runner of your own needs the same Build Tools as the hosted image.

### Variables to recreate

Settings → CI/CD → Variables. Mark them **protected** and protect the `v*` tags (Settings → Repository → Protected tags): a protected variable is only given to pipelines of protected refs, so an unprotected tag would build without its keys. Mask what GitLab accepts to mask, and keep every one of them protected either way.

| Variable                                                                 | Same as on GitHub       | Notes                                                                                                |
| ------------------------------------------------------------------------ | ----------------------- | ---------------------------------------------------------------------------------------------------- |
| `TAURI_SIGNING_PRIVATE_KEY`, `…_PASSWORD`                                | yes                     | the same key: the installed apps trust only its public half                                          |
| `SIGNTOOL_PFX_BASE64`, `SIGNTOOL_PFX_PASSWORD`                           | GitLab only             | the self-signed PFX the Windows job still signs with, see [SignPath](#windows-signing-with-signpath) |
| `APPLE_CERTIFICATE` … `APPLE_TEAM_ID`                                    | yes, optional           | none set = ad-hoc signing, as on GitHub; a partial set fails                                         |
| `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`, `R2_ACCOUNT_ID`, `R2_BUCKET` | yes                     | **required** here: R2 is the only host, a release without them fails                                 |
| `R2_PUBLIC_URL`                                                          | the repository variable | only if the domain is not `https://download.waveflow.app`                                            |
| `MACOS_RUNNER_TAG`                                                       | —                       | the tag of your own Mac runner, if not the hosted one                                                |
| `GITLAB_PUSH_TOKEN`                                                      | —                       | project access token, `write_repository`, for the cut-beta job                                       |

### What does not carry over

| On GitHub                                                                                                     | What happens on GitLab                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| ------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **release-please** opens the release PR, bumps, tags                                                          | GitHub-only, no GitLab support. Cut by hand: bump the version in every file [`release-please-config.json`](../release-please-config.json) lists — `package.json`, `src-tauri/crates/app/tauri.conf.json`, `src-tauri/crates/app/Cargo.toml`, `src-tauri/crates/core/Cargo.toml` — and `.release-please-manifest.json`, run `cargo check` to refresh `Cargo.lock`, write the `CHANGELOG.md` entry, commit `chore(main): release X.Y.Z`, then push a signed tag `vX.Y.Z`. The tag starts the release pipeline. A GitLab-aware tool can take this over later. |
| **Beta channel**: `cut-beta.yml`                                                                              | The `cut-beta` job: Run pipeline by hand with `BASE_VERSION`. Its tag push starts the release itself — a project access token's push does, unlike `GITHUB_TOKEN`'s. The stable-onto-beta copy is not needed (see [above](#how-the-app-reads-two-hosts)).                                                                                                                                                                                                                                                                                                   |
| **Downstream dispatches** (AUR, winget, COPR, apt)                                                            | Not ported; publish by hand or add jobs. Every one downloads from GitHub today: point `packaging/aur/PKGBUILD`'s `source` and `packaging/copr/waveflow.spec`'s `asset_url` at `https://download.waveflow.app/releases/v${version}/…`, and `apt-publish.yml`'s `gh release download` at the same files (its generator, `build-apt-repo.sh`, does not care where the tree goes: [APT repository](#apt-repository)). AUR and COPR push with their own keys, from anywhere. winget opens a pull request on `microsoft/winget-pkgs` on GitHub, which needs _a_ GitHub account — not necessarily this one — and `wingetcreate update` with the R2 URLs.                     |
| **Flathub**                                                                                                   | The manifest builds from the GitHub repository's URL: point it at the GitLab one in the Flathub repository.                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| **AppImage delta updates** (`appimage-update-info.sh`)                                                        | The string embedded in the image sends AppImageUpdate to GitHub's releases, so the GitLab build does not embed it and ships no `.zsync`. Images already out keep asking GitHub and lose delta updates; the in-app updater is unaffected. Bringing them back means the generic `zsync` transport, pointing at a fixed-name `.zsync` on R2.                                                                                                                                                                                                                  |
| **Install links**                                                                                             | The README, the website's download buttons and `packaging/flatpak/…metainfo.xml` link to GitHub releases and issues. Point them at waveflow.app and GitLab. The binaries also carry the GitHub URL in a few User-Agent strings and the Discord presence button; harmless, change them in the next release.                                                                                                                                                                                                                                                 |
| **Plugin store**                                                                                              | The registry comes from waveflow.app first, with raw.githubusercontent.com and jsDelivr as fallbacks; the plugin files are whatever the registry names. waveflow.app can repoint them without an app release.                                                                                                                                                                                                                                                                                                                                              |
| **CodeQL, CodeRabbit, labels, Dependabot, cache cleanup, `test-appimage.yml`, `flatpak-sources-refresh.yml`** | Not ported. GitLab's own SAST and dependency scanning, and Renovate for updates, are the nearest replacements.                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| **Build provenance** (`attest-build-provenance`)                                                              | No equivalent in the file. The minisign and Authenticode signatures still cover every payload.                                                                                                                                                                                                                                                                                                                                                                                                                                                             |

### The switch, in order

1. If GitHub still answers, stop pushing there, then disable `mirror-gitlab.yml`: it force-pushes, and would overwrite what lands on GitLab from then on. Disabling it stops no run already started or queued, so let those end (or cancel them and push what they carried by hand). Only then compare the backup's `main` and tags with GitHub's, or, if GitHub is gone, with your local clone's (the mirror's last run may not have finished), and push what the backup lacks before changing anything on GitLab. Turn the backup's CI/CD back on (Settings → General → Visibility, project features, permissions); that starts no pipeline, and every mirrored push carried `ci.skip`. Push a commit to `main` without `ci.skip` (an empty one will do) and check that the pipeline it starts runs green. Not Run pipeline: a pipeline started by hand runs only with `BASE_VERSION` set, and is then the beta cut.
2. Recreate the variables; the `v*` tags are already protected.
3. Register runners if the tier needs them.
4. Cut the next release by hand as above. Its manifest replaces `stable/latest.json` on R2, and every 2.0+ installation sees it.
5. Make the backup public, since it is now the project's home and the source Flathub and the downstream packages build from, and open the latest import's issues to the public (or move them into the backup) so issues can be read and filed again.
6. Update the install links, then the downstream packages one by one.

## Windows signing with SignPath

The Windows binaries are signed by [SignPath](https://signpath.org) (the SignPath Foundation's free certificate for open-source projects), so that Smart App Control stops blocking the installer (#610). The conditions the Foundation sets shape the pipeline: every release signature is approved by hand, the signed file must come from a GitHub-hosted build of this repository, only our own binaries are signed, and their product metadata is enforced.

### The two requests

SignPath signs a PE file or an MSI but cannot sign what sits inside an NSIS installer, so the Windows half of `release.yml` is the composite action [`.github/actions/windows-signed-bundle`](../.github/actions/windows-signed-bundle/action.yml), run from the workflow's own commit like the release scripts:

1. `tauri build --no-bundle` builds `waveflow.exe`, with no signing.
2. A first `tauri bundle --bundles nsis --no-binary-patching` is thrown away; its only use is the NSIS uninstaller, which the bundler hands to a `signCommand` ([`nsis-uninstaller-dispatch.mjs`](../scripts/release/nsis-uninstaller-dispatch.mjs)) that exports it. The dispatcher ignores every other file the bundler offers (the exe, the NSIS plugin DLLs, the setup).
3. **Approval 1 of 2**, artifact configuration `app`: `waveflow.exe` and the uninstaller.
4. The signed exe goes back to `target/release`, and `tauri bundle --no-binary-patching` builds the installers (nsis, and msi on a stable release). `--no-binary-patching` matters: the bundler would otherwise rewrite the exe it embeds and break its signature. The dispatcher puts the signed uninstaller in place, and only if its SHA-256 is the exported one.
5. **Approval 2 of 2**, artifact configuration `installers`: the NSIS setup and the MSI (the beta has none, hence `min-matches="0"`).
6. The updater `.sig` the bundler wrote covers unsigned bytes: it is deleted, and `tauri signer sign --app-version` recomputes it on the signed setup. The version is the one the bundler bound into the first signature, read back from it: `requireSignedVersion` refuses a payload that does not name it. Only then does `prepare-assets.py windows` run, so nothing derived from the setup (signature, manifest) exists before the last signature.
7. [`verify-updater-signature.mjs`](../scripts/release/verify-updater-signature.mjs) checks the new `.sig` the way the updater will, since `tauri signer` has no verify command: against the public key in `tauri.conf.json` (`plugins.updater.pubkey`), against the signed setup's bytes, and for the version it names (the tag's).
8. `Get-AuthenticodeSignature` on the setup, the MSI and the exe: a file without a signature fails the job. Under `release-signing` the status must also be `Valid`.

**The uninstaller** is signed because `makensis` writes it byte for byte the same whenever the installer script and its inputs do not change (measured: the same SHA-256 across two bundle runs, and with a different exe next to it). If it ever differs between the two passes, or cannot be exported, the job logs a warning under `test-signing` and goes on with an unsigned uninstaller; under `release-signing` it fails instead, since a release is never half signed. The NSIS plugin DLLs inside the setup are third-party code and are not signed, as the Foundation requires.

### What lives where

| Where                                                                                                  | What                                                                                                                    |
| ------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------- |
| Secret `SIGNPATH_API_TOKEN`                                                                            | the API token of a SignPath CI user                                                                                     |
| Variable `SIGNPATH_ORGANIZATION_ID`                                                                    | the organization id                                                                                                     |
| Variable `SIGNPATH_PROJECT_SLUG`                                                                       | the project's slug (the project is "WaveFlow")                                                                          |
| Variable `SIGNPATH_SIGNING_POLICY_SLUG`                                                                | `test-signing` or `release-signing`                                                                                     |
| [`.signpath/app.xml`](../.signpath/app.xml), [`.signpath/installers.xml`](../.signpath/installers.xml) | the two artifact configurations, pasted into SignPath under the slugs `app` and `installers`; keep both sides identical |

```bash
gh secret set SIGNPATH_API_TOKEN
gh variable set SIGNPATH_ORGANIZATION_ID --body "<organization id>"
gh variable set SIGNPATH_PROJECT_SLUG --body "<project slug>"
gh variable set SIGNPATH_SIGNING_POLICY_SLUG --body "test-signing"
```

A tag built by this workflow needs `@tauri-apps/cli` 2.12.0 or newer, the first with `tauri bundle --no-binary-patching`. The Windows job checks it before building anything, so a patch release cut from a maintenance branch that pins an older CLI stops at once with a message saying so, never half signed; the branch has to take the newer CLI first. (The 1.9 line already pins `^2.12.1`; no maintenance branch exists today.)

With any of them missing the Windows job fails on its first step, naming what is missing: a release is never published unsigned by accident.

### From the test certificate to the release one

Switching is a variable, not a change of code: once SignPath has issued the HSM certificate and `release-signing` is valid,

```bash
gh variable set SIGNPATH_SIGNING_POLICY_SLUG --body "release-signing"
```

From then on every release waits for two manual approvals in SignPath. While the Windows job waits (up to a few hours), the Linux and macOS jobs have already published: the GitHub release has no Windows installer yet and `latest.json` is not merged, so the updater's GitHub endpoint answers 404 until the Windows job ends. Approve promptly after tagging; the release page and the manifest complete themselves afterwards. The job's checks also expect Windows to accept the signature, and an unsigned uninstaller fails the job. Approve both requests within the job's 360 minutes (a request gives up after 150 minutes of waiting); if the job gives up, re-run the release with `workflow_dispatch` and the existing tag.

### Trying it without a release

[`windows-signing-check.yml`](../.github/workflows/windows-signing-check.yml) runs the same action and stops: no tag, no release, no upload, no R2. It also installs the setup silently on the runner and looks at the signature of the installed exe and uninstaller. It is started by hand from the Actions tab (**Run workflow**); use it to try a policy before a release depends on it. It makes real signing requests: under `release-signing` every run creates two that wait for approval in SignPath.

### On GitLab

[`.gitlab-ci.yml`](../.gitlab-ci.yml) does not use SignPath: the project's SignPath setup trusts GitHub.com builds only, and whether the Foundation's conditions would accept a GitLab build (SignPath does offer a GitLab.com trusted build system) has not been asked. Its `release-windows` job therefore keeps signing with the self-signed PFX (`SIGNTOOL_PFX_BASE64` and `SIGNTOOL_PFX_PASSWORD`), and a Windows build from the standby repository is again the kind Smart App Control may block. Fixing that means adding GitLab as a trusted build system in SignPath, if the Foundation allows it, or a certificate of our own.

## The AppImage is post-processed, and it has to stay that way

Tauri's AppImage bundler copies the webview's dependency closure into the AppDir, and that closure contains `libwayland-client.so.0` — a library the [official AppImage excludelist](https://github.com/AppImage/pkg2appimage/blob/master/excludelist) explicitly forbids bundling, because [Mesa breaks against a bundled copy](https://gitlab.freedesktop.org/mesa/mesa/-/issues/11316).

We build on Ubuntu. On any host with a newer Mesa — Fedora 44 ships Mesa 26 and libwayland 1.25 — the bundled copy wins the lookup and WebKit's WebProcess dies before painting anything:

```text
Could not create default EGL display: EGL_BAD_PARAMETER. Aborting...
```

The Rust side is unaffected: the app starts, creates its profile, opens its audio device, and shows an empty window. That asymmetry is why this was misread for months as a WebKitGTK 2.52 incompatibility, with users told to install a native package instead. It is not a WebKit problem, and it is not unfixable — [`scripts/fix-appimage.sh`](../scripts/fix-appimage.sh) removes the file (and `libva`, see [the GStreamer section](#the-appimage-carries-its-own-gstreamer)), repacks the squashfs with the compressor the bundler used, verifies the result re-extracts, and re-signs when an updater `.sig` is present, carrying over the version the build bound into the signature ([why](#how-the-app-reads-two-hosts)).

Both `release.yml` and `test-appimage.yml` run it right after `tauri build`. **If you ever restructure the Linux build, that step has to survive** — dropping it silently produces a release that installs fine and never opens, on exactly the distributions least likely to be in your test matrix. The script no-ops (and says so) if a future Tauri stops bundling the library, so it's safe to leave in place.

## The AppImage carries its own GStreamer

WebKitGTK plays every `<video>` in the interface — Canvas clips, animated album covers — through GStreamer. The AppImage bundles `libgstreamer-1.0.so.0` as part of the webview's dependency closure, and that library is **relocatable**: it looks for its plugins next to itself, in `usr/lib/gstreamer-1.0/` inside the AppDir, not in the host's plugin directory. Until 1.8.0 that directory did not exist, so the bundled GStreamer found no plugins at all, even on a host that had them installed:

```text
GStreamer element appsink not found. Please install it.
GStreamer element autoaudiosink not found. Please install it
(WebKitWebProcess): GLib-GObject-CRITICAL: g_signal_connect_data: assertion 'G_TYPE_CHECK_INSTANCE (instance)' failed
```

The web process then waits on a pipeline that was never built, and the whole interface freezes the first time a video is shown. Because the player resumes the last track on launch, a track with an animated cover froze the app again at every start. The released 1.7.0 AppImage had the same defect (it bundles Ubuntu's relocatable GStreamer 1.24). The native packages, the AUR, COPR and Flatpak builds were never affected: they use the system GStreamer.

`bundle.linux.appimage.bundleMediaFramework` in `tauri.conf.json` fixes it: the bundler copies the build machine's GStreamer plugins into the AppDir and points the runtime at them. **It bundles what the runner has installed**, so `release.yml` and `test-appimage.yml` install the plugin sets the webview needs:

| Package                     | Why                                                                                  |
| --------------------------- | ------------------------------------------------------------------------------------ |
| `gstreamer1.0-plugins-base` | `appsink`, `playbin`, `decodebin`, the converters WebKit builds on                   |
| `gstreamer1.0-plugins-good` | `qtdemux` (the `.mp4` container), `autoaudiosink`                                    |
| `gstreamer1.0-plugins-bad`  | the `va` hardware decoders (VA-API), used where the host driver decodes H.264 / HEVC |
| `gstreamer1.0-libav`        | the H.264 and HEVC decoders the clips and covers are encoded with                    |

Two more pieces make video actually play, and neither is packaging. The bundled GStreamer would otherwise share the host's registry cache (`~/.cache/gstreamer-1.0/registry.<arch>.bin`) with a different GStreamer version, so `preflight_appimage_gstreamer` in [`startup/preflight.rs`](../src-tauri/crates/app/src/startup/preflight.rs) points `GST_REGISTRY_1_0` at a file of its own when `APPIMAGE` is set. The same function points the plugin path at a fixed symlink, `~/.cache/app.waveflow/gstreamer-plugins`, re-aimed at the current mount on every launch (and left alone while it still leads into the mount of another instance that is running): the AppImage mounts under a new `/tmp/.mount_*` each time, and a registry keyed by those paths went stale at every launch, so GStreamer rescanned all its plugins the first time a video was shown and froze the interface for 3 to 10 seconds. Through the link, only an update of the AppImage costs a rescan. And WebKitGTK cannot play the asset protocol at all, on any Linux package: local clips go through a loopback HTTP server — see [ui.md](features/ui.md#track-canvas).

**`libva` is not bundled.** `fix-appimage.sh` removes it after the build, like `libwayland-client`: it loads the host's VA driver at run time, and the runner's older copy failed against Fedora's `iHD` driver (`vaInitialize: unknown libva error`, then `msdkcontext: Couldn't create a VA DRM display`). WebKit itself links `libdrm`, not `libva`, so the app starts either way; a host without `libva` only loses the decoders that need it. Whether a video is then decoded in hardware is the host driver's call: Fedora's own `libva-intel-media-driver` has no H.264 or HEVC (only `vavp9dec` registers), so on that machine a 1080p cover decodes in software, about half a core of an i7-1165G7, as it would in a native package.

Leave `gstreamer1.0-plugins-ugly` out: Tauri's own guidance warns that its licences make it hard to redistribute. Tauri also documents the flag as fully supported only on Ubuntu build systems, which is what the release runs on — a local AppImage built on another distribution is not a valid test of it. Use `test-appimage.yml`.

## Programs started from the AppImage get the host's environment back

The AppImage's launcher points `LD_LIBRARY_PATH`, `GIO_MODULE_DIR`, `GTK_PATH`, `GSETTINGS_SCHEMA_DIR`, `PATH`, `XDG_DATA_DIRS` and a few more into the mount, so that the bundled WebKitGTK finds its own libraries. Every program the app starts inherits them. The opener plugin's `xdg-open` then ran the host's `gio`, file manager and browser against the image's Ubuntu libraries, and they died on the first missing symbol. On Fedora, `gio` stopped at `undefined symbol: g_unix_mount_entry_get_options` and Firefox at `version NSS_3.126 not found`, so "Open log folder" did nothing. The native packages never set those variables.

[`external_open`](../src-tauri/crates/app/src/external_open.rs) is the one way the app opens a folder or a link. When `APPIMAGE` is set, it starts `xdg-open` with every entry under `$APPDIR` removed from the environment, and falls back to `gio open` when `xdg-open` cannot start or exits reporting no tool (3) or a failed open (4). It also withholds the variables the app set for the image itself: the GStreamer registry and plugin paths from the section above. They reach the mount through the cache-directory link, so nothing in their value names it. In any package, it withholds `GDK_BACKEND` the same way when the app set it to run through XWayland ([#894](https://github.com/InstaZDLL/WaveFlow/issues/894)), so a browser it starts is not pushed onto XWayland too; outside an AppImage, that is the only case it starts the launchers itself. Otherwise it hands over to the plugin unchanged. The frontend's links go through its `open_external_url` command, which accepts `http`, `https` and `mailto` only, because plugins pass their own links there. Revealing a track in its folder stays on the plugin: it asks the file manager over D-Bus, which starts it with its own environment.

## AppImage delta updates: three steps in one order

The AppImage runtime reserves a 1024-byte `.upd_info` section for a single string saying where updates come from. `appimagetool -u` fills it; Tauri builds the AppImage itself and exposes no equivalent, so it shipped zeroed and every ecosystem tool — AppImageUpdate, AppImageLauncher, AM, AppManager — reported _no update information available_ ([#527](https://github.com/InstaZDLL/WaveFlow/issues/527)).

[`scripts/appimage-update-info.sh`](../scripts/appimage-update-info.sh) writes it:

```text
gh-releases-zsync|InstaZDLL|WaveFlow|latest|WaveFlow_*_linux-x86_64.AppImage.zsync
```

`latest` is a literal the transport resolves through the GitHub API on every check, so the string stays correct for all future versions. Paired with the `.zsync` control file `zsyncmake` emits, a client reuses what it already has and fetches only the changed blocks — measured on a synthetic release: **3 080 192 bytes reused locally, 65 774 fetched**, reconstructed file identical to the published one.

The section is written with `dd conv=notrunc` at the offset `readelf` reports, never `objcopy --update-section`. This file is an ELF _with a squashfs appended_ that the runtime locates by a fixed offset; rebuilding the container would move that boundary and produce an image that no longer mounts. The script asserts the file size and the `--appimage-offset` are unchanged afterwards, so that mistake cannot ship quietly.

**The order is load-bearing, and all three steps mutate or describe the same bytes:**

1. `fix-appimage.sh` — repacks the squashfs (keeps the runtime header verbatim, so anything written into it survives)
2. `appimage-update-info.sh` — writes `.upd_info`, then re-signs, because the `.sig` covers the bytes it just changed
3. `zsyncmake` — runs **last**, on the renamed `dist-release/` copy

Step 3's placement matters twice. Its checksums describe the payload byte for byte, so any later rewrite would silently invalidate it. And the control file names its target, which clients resolve _relative to the URL they fetched the `.zsync` from_ — both are assets of the same release, so the bare renamed filename lands on the right image, while the bundler's own name would send every client to a 404.

**If you rename the Linux release assets, the embedded pattern has to move with them.** `WaveFlow_*_linux-x86_64.AppImage.zsync` lives in the script, the asset name lives in `release.yml`, and they are two halves of one contract: rename one and clients ask for a `.zsync` that was never published — visible to users, invisible in CI. Step 3 therefore reads the string back out of the image it is about to ship and fails the release when the two disagree. That check fires on a tag, which is a poor moment to discover it, so treat it as a backstop and change both together.

> **Stable only.** The update information points at the repository's latest release, and GitHub excludes pre-releases from "latest" — a beta carrying that string would walk its user _back_ to the newest stable. Beta builds keep an empty section, so those clients honestly report no update information and the [in-app beta channel](#beta-channel) stays the only path.

## Flatpak sources are generated, and they go stale silently

Flathub builds with the network disabled, so **every crate and npm tarball must be pre-declared** in [`packaging/flatpak/generated/`](../packaging/flatpak/generated/) by [`generate-sources.sh`](../packaging/flatpak/generate-sources.sh).

A `Cargo.lock` / `package.json` bump that skips that regeneration does **not** fail CI — it fails much later, inside Flathub's sandbox, on a crate nobody can download. `chore: update dependencies` (23be643) left **53** crates undeclared exactly this way.

[`check-sources.py`](../packaging/flatpak/check-sources.py) guards the invariant on every PR touching a lockfile. It verifies **coverage** — every registry crate in `Cargo.lock` has a source entry, every npm package in the Flatpak lockfile has a tarball — offline, in about a second.

It deliberately does **not** regenerate-and-diff: the generator runs `npm install --package-lock-only`, which re-resolves `^` ranges against the live registry, so such a check would go red whenever any transitive dependency publishes. Freshness is instead a monthly chore ([`flatpak-sources-refresh.yml`](../.github/workflows/flatpak-sources-refresh.yml), also `workflow_dispatch`-able) that regenerates and opens a PR.

> **Toolchain split.** The app builds from `bun.lock`, but the Flatpak sandbox runs `npm ci --offline` against a **separate npm lockfile** (`flatpak-node-generator` can't read `bun.lock`). A dependency pin that must hold for shipped builds therefore belongs in `package.json` — e.g. an `overrides` entry — not in `bun.lock` alone.

> **The manifest's own paths go stale the same way.** No CI job runs `flatpak-builder`, and `check-sources.py` only reads the lockfiles, so nothing checks the repository paths the [manifest](../packaging/flatpak/app.waveflow.WaveFlow.yaml)'s `build-commands` install from — the binary, the icons, the desktop entry. The icon step kept reading `src-tauri/icons/` long after the workspace split moved them under `src-tauri/crates/app/icons/`. Moving or renaming anything under `src-tauri/crates/app/` means grepping the manifest for it.

## Flathub

WaveFlow is not on Flathub yet. Everything a submission needs is in [`packaging/flatpak/flathub/`](../packaging/flatpak/flathub/), and its [README](../packaging/flatpak/flathub/README.md) is the procedure, command by command: first submission, domain verification, and the PR each release after that. What follows is why it is shaped that way.

**Two manifests, one content.** [`packaging/flatpak/app.waveflow.WaveFlow.yaml`](../packaging/flatpak/app.waveflow.WaveFlow.yaml) builds the working tree through a `type: dir` source, for local tests. Flathub refuses a `dir` source, and its build sandbox refuses any local path outside the manifest's directory, so the copy in `flathub/` reads the repository at a `tag` plus the `commit` it points to (flatpak-builder checks the two agree, so a moved tag cannot change what ships), and reads `cargo-sources.json` / `node-sources.json` from the root of the Flathub repository, where they are copied **from the tag**. Nothing else may differ: a change to one manifest is made to the other in the same commit.

**What Flathub builds is the tag.** The metainfo, the desktop entry, the icons and the npm lockfile are installed from the WaveFlow checkout at the tag, so they have to be right **before** tagging:

- **The metainfo's release entry.** release-please bumps `package.json`, `tauri.conf.json` and the crates, not [`app.waveflow.WaveFlow.metainfo.xml`](../packaging/flatpak/app.waveflow.WaveFlow.metainfo.xml), whose `<releases>` stopped at 1.3.0. Flathub shows the newest entry as the app's version, so each release adds its `<release version="…" date="…">` in the release-please PR. A date cannot be left out: the linter refuses a release without one.
- **The generated sources.** `check-sources.py` passing on the release commit, as in the section above. A stale list fails inside Flathub's offline build, after the tag.
- **The screenshots.** The metainfo points at `docs/screenshots/` on `main`; Flathub copies them to its own host at each build, so a later change on `main` reaches the store only with the next release.

**The build that Flathub runs.** `flathub-build` (inside `org.flatpak.Builder`) is `flatpak-builder --sandbox --force-clean --install-deps-from=flathub --mirror-screenshots-url=https://dl.flathub.org/media --repo=repo builddir <manifest>`: no network once the sources are downloaded, no path outside the manifest's directory. The local manifest therefore cannot go through it (its `dir` source is two levels up); to test the Flathub copy before a tag exists, point its `url` at a bare clone placed **inside** the Flathub checkout (`url: file:///…/flathub/WaveFlow.git`) and set `tag` / `commit` to a test tag there. The manifest linter then reports `module-waveflow-source-git-url-not-http` for that URL, and nothing else.

**Runtime.** Flathub accepts a new app only on the latest runtime, and its linter warns as soon as a newer one is out. GNOME 51 still ships `webkit2gtk-4.1` beside `webkitgtk-6.0`; a runtime that drops 4.1 cannot be adopted until Tauri moves to WebKitGTK 6 ([B1](upstream-blockers.md#b1--webkit2gtk-41--gtk3-chrome-on-linux)).

**Features and resources.** The build passes `--no-default-features --features sync_v2`: the updater is compiled out (`flatpak update` owns updates), the WaveFlow Server source is kept. A feature added to `default` in `crates/app/Cargo.toml` is added to that list too, or the Flatpak silently lacks it. `--no-bundle` also skips `bundle.resources`, so the manifest copies the bundled plugins itself into `/app/lib/WaveFlow`, where Tauri looks for resources next to `/app/bin`; a resource added to `tauri.conf.json` outside `resources/plugins/` needs a line there too.

**Permissions.** Each one is justified by a comment in the manifest, and the PR has to explain them again. Folders other than `~/Music` come through the FileChooser portal (GTK3's native chooser goes there inside a Flatpak), so the manifest asks for none; a `--talk-name` for a portal is refused by the linter, since portals need no permission. Flatpak lets the app own only bus names under its app ID, and two of WaveFlow's are not: MPRIS's `org.mpris.MediaPlayer2.waveflow` and the single-instance plugin's `app.waveflow.SingleInstance` (the Tauri identifier, not the app ID). Without the second, the plugin logs that the bus is unavailable and every launch starts another copy on the same databases. The ALSA exclusive output and its DoP path need the raw `/dev/snd` devices, which the Flatpak does not ask for: there, playback goes through the sound server.

**Architectures.** [`flathub.json`](../packaging/flatpak/flathub/flathub.json) limits the build to x86_64, the only Linux architecture any WaveFlow build has run on. Dropping it makes Flathub build aarch64 as well, and a failure there fails the whole update.

**After the merge.** Flathub creates `flathub/app.waveflow.WaveFlow` and invites the submitter. Verification proves the app ID's domain: the token from the Developer Portal goes in `https://waveflow.app/.well-known/org.flathub.VerifiedApps.txt`, served by the `waveflow-landing` site.

## macOS signing and the TCC prompt

Signing the macOS bundle is not only about Gatekeeper. It is what stops macOS from asking for folder permission **on every single launch**.

macOS records a TCC grant — the "WaveFlow would like to access files in your Downloads folder" consent — against the app's _designated requirement_, i.e. its code-signing identity. Before this was wired up, the shipped bundle carried only the ad-hoc signature the linker adds automatically on Apple Silicon:

```
Identifier=waveflow-36c95c4d36c8f6a6      ← generated, not app.waveflow
Signature=adhoc
flags=0x20002(adhoc,linker-signed)
Info.plist=not bound
Sealed Resources=none
```

There is no `Contents/_CodeSignature` in that state: the `.app` was never passed to `codesign`, only its Mach-O binary was. With no sealed bundle and no stable identifier, there is nothing to anchor the grant to, so it never persists. Any user whose library lives under `~/Downloads`, `~/Desktop`, `~/Documents`, an external drive or a network share re-consents at every launch.

Signing the bundle fixes it, and **ad-hoc is enough for this specific problem** — `codesign -s -` still produces sealed resources, a bound `Info.plist` and the `app.waveflow` identifier from the bundle. What ad-hoc does _not_ do is survive an update (each build is a different identity, so the grant resets on upgrade) or satisfy Gatekeeper. A Developer ID identity is stable across versions and does both.

Three pieces make this work, and they have to stay together:

- [`Info.plist`](../src-tauri/crates/app/Info.plist) — the `NS*UsageDescription` purpose strings macOS shows in the consent dialog. Auto-merged by the bundler because it sits next to `tauri.conf.json`.
- [`waveflow.entitlements`](../src-tauri/crates/app/waveflow.entitlements) — signing turns **on** hardened runtime, which the old linker-signed bundle never had, so signing introduces a failure mode rather than only removing one. The wasmtime plugin host executes Cranelift output from anonymous `mmap` + `mprotect` pages, not `MAP_JIT`, so it needs `com.apple.security.cs.allow-unsigned-executable-memory`; `com.apple.security.cs.allow-jit` alone is **not** enough and the process is SIGKILL'd (`Namespace CODESIGNING, Code 2, Invalid Page`) the moment a plugin instantiates. Web Radio ships in the bundle, so that path runs for every user. Verified against the pinned source, not assumed: wasmtime 47.0.3 contains no `MAP_JIT` at all and `Mmap::make_executable` goes through plain `mprotect`, so no narrower grant exists — re-evaluate only if the plugin host changes how it maps executable memory. The companion `allow-jit` is **not** redundant: JavaScriptCore runs in-process for the WKWebView and JITs via `MAP_JIT`. The two entitlements serve two different consumers, so neither can be pruned as dead weight.
- The `Resolve macOS signing mode` + `Verify macOS signature` steps in [`release.yml`](../.github/workflows/release.yml).

Signing happens **inside** `tauri build`, driven by `APPLE_SIGNING_IDENTITY`, not as a post-build `codesign` pass. That ordering is not cosmetic: the bundler builds the DMG and the `.app.tar.gz` updater payload _from_ the `.app`, so signing afterwards would leave both wrapping an unsigned copy.

The verify step asserts the sealed-resources / `app.waveflow` / designated-requirement triple, because a silent signing failure would look like a clean release and quietly bring the per-launch prompt back.

### The one-command installer, until there is a Developer ID

Ad-hoc signing leaves the Gatekeeper half open: a `.dmg` downloaded by a browser carries `com.apple.quarantine`, and macOS refuses to open a quarantined app that is not notarized, with a dialog that offers no way through on macOS 15 and later short of System Settings → Privacy & Security → Open Anyway. [`scripts/install-macos.sh`](../scripts/install-macos.sh) is the way around it that does not ask users to lower a system setting:

```sh
curl -fsSL https://waveflow.app/install.sh | bash               # stable
curl -fsSL https://waveflow.app/install.sh | bash -s -- --beta  # beta channel
```

- **What it installs**: `WaveFlow_<version>_macos-universal.app.tar.gz`, the updater payload, not the DMG. Both wrap the same signed `.app`, but the archive needs no disk image mounted (one that `hdiutil` can leave attached when a script stops halfway), and GitHub publishes a SHA-256 digest for it. The version comes from the channel's updater manifest (`latest.json`, or `latest-beta.json` for `--beta`), so the script installs what the in-app updater would offer on that channel; `--version X.Y.Z` names one.
- **What it checks**: the archive's SHA-256 against the `digest` the GitHub API reports for the asset (skipped with a warning when the API does not answer, as it stops doing after 60 anonymous calls an hour); that the archive holds `WaveFlow.app` and nothing else; that the bundle is `app.waveflow` at the version asked, and that the Mac meets its `LSMinimumSystemVersion`; and `codesign --verify --strict` with the sealed resources and the `app.waveflow` identifier that `release.yml` asserts before publishing, so a damaged file is refused. What it cannot check is _who_ built it: an ad-hoc signature carries no identity, and the updater's `.sig` is a minisign signature, which needs a tool macOS does not ship. Integrity rests on GitHub over HTTPS.
- **What it changes**: `xattr -dr com.apple.quarantine` on the `WaveFlow.app` it is about to install, after saying why, and nothing else — never `spctl --master-disable` or any other global setting. A file `curl` writes is normally not quarantined in the first place, so this mostly guards against a copy that picked the flag up anyway; `--no-unquarantine` skips it. `sudo` is asked for only to write into `/Applications` when the account cannot (the copy is then handed back to the user, as a dragged copy would be, so the in-app updater can replace it later); `--user` installs into `~/Applications` instead, `--dir` into any existing folder. A WaveFlow running from the folder being written stops the script, or is asked to quit with `--force`; one running from another copy is left alone, and the new copy is not opened over it. `--uninstall` removes the app and prints where the data is, without deleting it.
- **How it is tested**: [`scripts/tests/install-macos.test.sh`](../scripts/tests/install-macos.test.sh) runs it on Linux with fakes for every macOS tool and for GitHub — version choice, refusals, the quarantine step, cleanup after a failure — and `--live` adds one real download of the current release. The `macOS installer` job of `ci.yml` runs ShellCheck and the bench on every pull request. Neither replaces a run on a Mac, which is how it was first checked (macOS 26, Apple silicon, the system's bash 3.2): install into a scratch `--dir`, the copy opening with no Gatekeeper dialog, `--force` quitting a running copy, `--no-unquarantine`, `--uninstall`. Not run there: the `sudo` path into a `/Applications` the account cannot write, and a download that arrives quarantined — `curl` never sets the flag, so `xattr -dr` was only checked on a copy flagged by hand. Opening a newer WaveFlow migrates the libraries an older one reads, so a test that launches the app needs a `HOME` of its own (`open -n --env HOME=…`).
- **How it is served**: the script lives here, once. `waveflow.app/install.sh` is a redirect, configured in the site's repository, to `https://raw.githubusercontent.com/InstaZDLL/WaveFlow/main/scripts/install-macos.sh` (`curl -L` follows it). The file runs from `main()`, called on its last line, so a download cut short executes nothing.

The real fix is the one above: a **Developer ID** certificate plus **notarization**, after which a browser download opens like any other app and this script becomes a convenience rather than a workaround.

## Notes

- **Dev builds skip the updater entirely** (gated on
  `cfg(not(debug_assertions))` in `lib.rs`). You will not see
  update prompts during `bun run tauri dev`.
- **Windows install mode** is `passive` — the user sees a brief
  installer GUI, no clicks needed. Switch to `quiet` for fully
  silent (less obvious to the user) or `basicUi` for the standard
  NSIS dialog.
- **Windows Authenticode signing** goes through SignPath, see
  [Windows signing with SignPath](#windows-signing-with-signpath). Until
  the SignPath Foundation certificate is issued (policy
  `release-signing`) the builds are signed by the test certificate,
  which no Windows trusts. Smart App Control blocks the app whenever
  Microsoft's online reputation service has no positive verdict for a
  build signed by a certificate outside the Trusted Root Program, which
  is why it failed on some days and worked on others (#610).
- **What fixes it** is a code-signing certificate from a CA in the
  Trusted Root Program: with that, Smart App Control allows the app
  even without any reputation. EV is no shortcut: SmartScreen has
  treated EV and OV the same since 2024. Every executable file that
  ships must carry the signature, not only the installer: the app
  binary and the uninstaller too. The NSIS plugin DLLs the setup
  unpacks into its temporary folder stay unsigned: they are third-party
  binaries, which the SignPath Foundation forbids signing. How Smart
  App Control treats them is to be tested on a machine with it on, once
  the release certificate is issued. To check a release,
  `Get-AuthenticodeSignature` on the installer and on the installed
  `waveflow.exe` must report `Valid`; the SignPath test certificate
  reports `UnknownError` (untrusted root).

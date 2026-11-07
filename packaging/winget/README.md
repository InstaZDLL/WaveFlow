# Winget distribution

WaveFlow is published to the [Microsoft Windows Package Manager
Community Repository](https://github.com/microsoft/winget-pkgs) as
**`InstaZDLL.WaveFlow`** so Windows users can install / upgrade /
uninstall via the native `winget` CLI:

```pwsh
winget install InstaZDLL.WaveFlow
winget upgrade  InstaZDLL.WaveFlow
winget uninstall InstaZDLL.WaveFlow
```

Both the NSIS `.exe` (per-user install under `%LOCALAPPDATA%`) and
the MSI (machine-wide install under `Program Files`) are listed —
winget picks the right one based on `--scope user|machine`.

## How releases land in winget

The flow is fully automated for every tagged release. Once
[`.github/workflows/release.yml`](../../.github/workflows/release.yml)
finishes uploading the `*-setup.exe` and `*.msi` artefacts to GitHub
Releases, [`.github/workflows/winget.yml`](../../.github/workflows/winget.yml)
fires on the `release: published` event and:

1. downloads both Windows installers from the release,
2. computes their SHA-256 digests,
3. fills the manifest: the new version, URLs and hashes, on top of the
   manifest **last published in winget-pkgs**, whose locale strings
   (description, tags, privacy link) carry over unchanged,
4. validates locally via `winget validate`,
5. forks `microsoft/winget-pkgs` under the maintainer's account,
6. opens a PR with the new `manifests/i/InstaZDLL/WaveFlow/<v>/` dir.

A Microsoft "Service-and-Reliability" bot reviews the PR — when the
manifest is clean and the installers match their declared hashes,
the bot auto-approves and merges, usually within minutes. Manual
reviewer follow-up only happens when something is off (signature
mismatch, broken URL, schema violation).

## First-time setup (one-off)

This directory carries the **v1.0.0 manifest** committed at first
release. Before the auto-submit workflow can run, two prerequisites
must be in place:

### 1. Generate the WINGET_PAT secret

The action runs `wingetcreate submit`, which requires GitHub API
write access to fork `microsoft/winget-pkgs` and push a branch with
the manifest. Create a Personal Access Token (classic) with
`public_repo` scope:

1. <https://github.com/settings/tokens/new?scopes=public_repo&description=WaveFlow%20winget%20auto-submit>
2. Generate, copy.
3. Push it to the repo secrets:

   ```pwsh
   gh secret set WINGET_PAT --body "<paste-the-token>"
   ```

### 2. Hand-submit the v1.0.0 manifest (only the very first time)

The auto-submit workflow can update an existing winget package, but
the very first submission of a brand-new identifier (`InstaZDLL.WaveFlow`)
benefits from a manual PR so a human reviewer can confirm the
publisher attribution. After that, all subsequent versions land via
the workflow with no manual step.

```pwsh
# From a Windows machine with Git and the .NET SDK:
git clone https://github.com/<your-user>/winget-pkgs.git
cd winget-pkgs

# Copy this directory's contents into the right path:
$src = "<path-to-WaveFlow>/packaging/winget/1.0.0"
$dst = "manifests/i/InstaZDLL/WaveFlow/1.0.0"
New-Item -ItemType Directory -Path $dst -Force | Out-Null
Copy-Item "$src/*.yaml" $dst

git checkout -b InstaZDLL.WaveFlow-1.0.0
git add manifests/i/InstaZDLL/WaveFlow/1.0.0
git commit -m "New version: InstaZDLL.WaveFlow 1.0.0"
git push origin InstaZDLL.WaveFlow-1.0.0

# Then open a PR against microsoft/winget-pkgs (the bot bots-bots
# verifies the SHA-256 against the URLs and auto-merges if clean).
```

## Template maintenance

The `1.0.0/` directory is the manifest of the first submission, kept as
the **reference** for the package's wording. The workflow does not read
it: each release copies the locale strings of the manifest last
published in winget-pkgs. So when the description, the tags or the
privacy link change, the change reaches winget only by hand, once:

1. update `InstaZDLL.WaveFlow.locale.en-US.yaml` here, so the reference
   says it;
2. when the workflow opens the PR for the next version in
   `microsoft/winget-pkgs`, edit that PR's
   `InstaZDLL.WaveFlow.locale.en-US.yaml` (keep its `PackageVersion`)
   with the same fields before it merges; every later release then
   carries them over.
3. if the bot merged that PR first, open one more PR in
   `microsoft/winget-pkgs` that changes only
   `manifests/i/InstaZDLL/WaveFlow/<version>/InstaZDLL.WaveFlow.locale.en-US.yaml`
   for the version just published; a metadata-only change to a
   published version is accepted, and the next release copies it.

The schema version is pinned to `1.6.0` (the lowest version that
supports both `nullsoft` and `wix` installer types in the same
manifest). Bump to the newer schema only when winget validation
warns about deprecated fields.

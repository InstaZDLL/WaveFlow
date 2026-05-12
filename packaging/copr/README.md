# COPR distribution

WaveFlow is published to [Fedora COPR](https://copr.fedorainfracloud.org/)
as **`InstaZDLL/waveflow`** so Fedora / RHEL / CentOS Stream / Rocky /
Alma users can install / upgrade through their native `dnf`:

```bash
sudo dnf copr enable instazdll/waveflow
sudo dnf install waveflow
```

Updates land automatically alongside every other system update once
the COPR repo is enabled.

## How releases land in COPR

Same tag-driven flow as the AUR + winget pipelines. After
[`.github/workflows/release.yml`](../../.github/workflows/release.yml)
has uploaded the `WaveFlow_<v>_linux-x86_64.rpm` to GitHub Releases,
the final `updater-manifest` job dispatches
[`.github/workflows/copr.yml`](../../.github/workflows/copr.yml),
which:

1. spins up a Fedora 44 container,
2. bumps `Version:` in [`waveflow.spec`](./waveflow.spec) to the new
   tag (in-place — the committed spec stays at the last shipped
   version),
3. runs `spectool -gR` to download the freshly-released
   `WaveFlow_<v>_linux-x86_64.rpm` into the SRPM,
4. wraps it as a source RPM with `rpmbuild -bs`,
5. uploads the SRPM to COPR via `copr-cli build InstaZDLL/waveflow ...`.

COPR mocks the SRPM in every chroot configured on the project (Fedora
39+, EPEL 9, etc.) and serves the resulting binary RPM under
`https://download.copr.fedorainfracloud.org/results/instazdll/waveflow/`.

## How the spec works (binary repackaging)

The spec **repackages** the upstream RPM produced by `tauri-bundler`
instead of building from source. `tauri-bundler` only runs on a Linux
host with the right `webkit2gtk-4.1` headers, and Fedora ships
`webkit2gtk4.1` under a different soname than Ubuntu — driving a real
source build for an OSS Tauri app is more work than the value it
adds for a Linux-side respin. So instead the spec:

- declares the upstream `.rpm` URL as `Source0`,
- skips auto-detected Requires (`AutoReqProv: no`) since they reference
  Debian sonames,
- restates the dependencies under Fedora-native package names
  (`webkit2gtk4.1`, `libayatana-appindicator-gtk3`, …),
- in `%install`, runs `rpm2cpio | cpio -idmv` to extract the binary
  blob directly into the buildroot.

This is the same pattern used by Spotify, Discord, and other binary
Linux apps that ship to COPR. The Fedora project itself wouldn't
accept this in `Fedora/`, but COPR is for user-hosted repos and
explicitly allows it.

## First-time setup (one-off)

The auto-publish workflow can't bootstrap the COPR project — it needs
to exist before `copr-cli build` is called. Steps:

### 1. Sign up to FAS (Fedora Account System)

Free, no Fedora install needed: <https://accounts.fedoraproject.org/>.

### 2. Create the COPR project

1. Visit <https://copr.fedorainfracloud.org/coprs/add/>
2. Project name: `waveflow`
3. Description: paste the `%description` block from the spec
4. Build chroots: at minimum
   - `fedora-rawhide-x86_64`
   - `fedora-44-x86_64` (current stable)
   - `fedora-43-x86_64`
   - `fedora-42-x86_64`
   - `epel-9-x86_64` (for RHEL 9 / Rocky 9 / Alma 9 / CentOS Stream 9)
5. **External repositories**: leave empty — all dependencies come
   from the default Fedora / EPEL repos already configured per chroot.
6. **Build options → "Auto-prune outdated builds"**: enable, retention
   = 3 versions (keeps the COPR storage footprint sane).
7. Save.

### 3. Generate the API token

1. Visit <https://copr.fedorainfracloud.org/api/>
2. Copy the `login` and `token` fields shown on that page
3. Push them to repository secrets:

   ```pwsh
   gh secret set COPR_LOGIN --body "<the-login-value>"
   gh secret set COPR_TOKEN --body "<the-token-value>"
   ```

   Token lifetime: 6 months by default. When the workflow starts
   failing with `401 Unauthorized` past that mark, regenerate the
   token on the same page and re-push the secrets.

### 4. (Optional) Manual first build to sanity-check the chroots

```bash
# From any Fedora box, with the same ~/.config/copr file as the workflow
# generates from the secrets:
gh release download v1.0.0 -p "WaveFlow_*linux-x86_64.rpm" -D /tmp
fedpkg --release f44 srpm packaging/copr/waveflow.spec
copr-cli build InstaZDLL/waveflow /tmp/waveflow-1.0.0-1*.src.rpm
```

After the first manual build, every subsequent release will go
through the automated workflow with no manual step.

## Adding more architectures later

Tauri-bundler currently only emits an x86_64 `.rpm` because that's
what the [`release.yml`](../../.github/workflows/release.yml) matrix
builds. When the matrix gets an `aarch64` runner, drop the
`ExclusiveArch: x86_64` line from the spec, add an `aarch64` asset
URL pattern, and add `fedora-*-aarch64` + `epel-9-aarch64` chroots
to the COPR project.

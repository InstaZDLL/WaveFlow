# Flathub submission

The files Flathub needs, kept in the WaveFlow repository so they change with the code. Nothing here is built by CI, and nothing is sent to Flathub automatically. Background and the rules behind each step: [`docs/RELEASING.md`, "Flathub"](../../../docs/RELEASING.md#flathub).

| File                              | Goes to the Flathub repository as                                                                   |
| --------------------------------- | --------------------------------------------------------------------------------------------------- |
| `app.waveflow.WaveFlow.yaml`      | `app.waveflow.WaveFlow.yaml`, with `commit` filled in                                               |
| `flathub.json`                    | `flathub.json` (x86_64 only: WaveFlow has never been built for aarch64 Linux)                       |
| `../generated/cargo-sources.json` | `cargo-sources.json`, taken **at the tag**                                                          |
| `../generated/node-sources.json`  | `node-sources.json`, taken **at the tag**                                                           |
| (none)                            | `shared-modules`, the [flathub/shared-modules](https://github.com/flathub/shared-modules) submodule |

The manifest is [`../app.waveflow.WaveFlow.yaml`](../app.waveflow.WaveFlow.yaml) with its sources changed and nothing else: the repository at a tag instead of the working tree, and the two generated lists read from the root of the Flathub repository. A change to one is made to the other in the same commit.

## Before the tag

These land in the release commit, since Flathub builds what the tag points to:

1. **The release entry.** Add `<release version="2.0.0" date="YYYY-MM-DD">` at the top of `<releases>` in [`../app.waveflow.WaveFlow.metainfo.xml`](../app.waveflow.WaveFlow.metainfo.xml), in the release-please PR, dated the day it merges. release-please does not touch this file, and Flathub shows the newest entry as the app's version.
2. **Fresh generated sources.** `py -3 packaging/flatpak/check-sources.py` must pass on the release commit. If `Cargo.lock` or `package.json` moved since the last refresh, run the `flatpak-sources-refresh` workflow and merge its PR first.

## Filling in `commit`

`commit` is the commit the tag points to; flatpak-builder refuses the build when the two disagree. Once `v2.0.0` exists, in the Flathub checkout, with `W` a WaveFlow clone:

```bash
W=/path/to/WaveFlow
git -C "$W" fetch --tags origin
sha=$(git -C "$W" rev-parse --verify 'v2.0.0^{commit}') &&
  sed -i "s/^\( *commit: \).*/\1\"$sha\"/" app.waveflow.WaveFlow.yaml
grep -n -A3 'type: git' app.waveflow.WaveFlow.yaml   # 40 hex digits, not 000…
```

Keep the quotes: a hash made only of digits would otherwise be read as a number.

## First submission

1. Fork and branch, from `new-pr` (not `master`):

   ```bash
   gh repo fork --clone flathub/flathub && cd flathub
   git checkout --track origin/new-pr
   git checkout -b add-waveflow new-pr
   ```

2. Copy the files, from the tag:

   ```bash
   W=/path/to/WaveFlow
   git -C "$W" show v2.0.0:packaging/flatpak/flathub/app.waveflow.WaveFlow.yaml > app.waveflow.WaveFlow.yaml
   git -C "$W" show v2.0.0:packaging/flatpak/flathub/flathub.json > flathub.json
   git -C "$W" show v2.0.0:packaging/flatpak/generated/cargo-sources.json > cargo-sources.json
   git -C "$W" show v2.0.0:packaging/flatpak/generated/node-sources.json > node-sources.json
   git submodule add https://github.com/flathub/shared-modules.git shared-modules
   ```

   Then fill in `commit` as above.

3. Build and lint it the way Flathub does (network off once the sources are downloaded; about a quarter of an hour on 8 cores, eight minutes of it in cargo):

   ```bash
   flatpak install --user -y flathub org.flatpak.Builder
   flatpak run --command=flathub-build org.flatpak.Builder --install app.waveflow.WaveFlow.yaml
   flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest app.waveflow.WaveFlow.yaml
   flatpak run --command=flatpak-builder-lint org.flatpak.Builder repo repo
   flatpak run app.waveflow.WaveFlow
   ```

   `flathub-build` leaves `builddir/`, `repo/` and `.flatpak-builder/` behind (about 6 GB): delete them before committing, they must not be in the PR. `flatpak run` uses the app's real data folder, `~/.var/app/app.waveflow.WaveFlow`, and migrates whatever an earlier build left there: move it aside first if it holds anything to keep.

4. Commit only the five entries of the table above, push, and open the PR against **`new-pr`**, titled **`Add app.waveflow.WaveFlow`**. Fill in the PR template yourself: Flathub closes PRs that drop the template or read as generated text. Explain each permission (the manifest's comments say what needs each one):

   - `--share=network`: online lookups (lyrics, artist data, MusicBrainz, scrobbling), podcasts and web radio, the plugin store, WaveFlow Server, WebDAV and SMB shares, the opt-in DLNA and MPD servers. An in-app offline mode turns it all off.
   - `--filesystem=xdg-music`: the default library, written to by the tag editor, lyrics sidecars and dropped files copied into `~/Music/WaveFlow`. Other folders come through the FileChooser portal.
   - `--own-name=org.mpris.MediaPlayer2.waveflow`: MPRIS media controls, under the name the app registers.
   - `--own-name=app.waveflow.SingleInstance`: the single-instance lock of Tauri's plugin, named after the Tauri identifier `app.waveflow`. Without it, opening a file while WaveFlow runs starts a second copy on the same databases.
   - `--talk-name=org.kde.StatusNotifierWatcher`: the tray icon.
   - `--talk-name=org.freedesktop.Notifications`: track-change notifications (off by default).
   - `--filesystem=xdg-run/app/com.discordapp.Discord:create` and `xdg-run/discord-ipc-0..3`: Discord Rich Presence's socket, from the Discord Flatpak or a host install (on by default, a switch in Settings).
   - `--socket=pulseaudio`, `--device=dri`, `--socket=wayland`, `--socket=fallback-x11`, `--share=ipc`: sound and display.

   One thing a reviewer may ask about: the bundled Web Radio plugin is installed as the WebAssembly file committed in `src-tauri/resources/plugins/web-radio/`, built in its own repository, not compiled by this manifest. Its source is in `src-tauri/plugins/web-radio/`, but it builds for `wasm32-wasip2`, and the `rust-stable` SDK extension ships the standard library for x86_64 and i686 only (checked on its 26.08 branch). Flathub asks for everything to be built from source; if the review holds to that, the choices are an exception for this file, or leaving it out of the Flatpak, where Web Radio would then come from the plugin store.

5. Answer the review in the same PR (never close it, never merge `master` into it). Once the comments are settled, comment `bot, build` for a test build.

## After the merge

Flathub creates `flathub/app.waveflow.WaveFlow` and invites the submitter as a collaborator: accept the invite, it expires.

**Verify the domain**, so the app shows as verified:

1. Log in at <https://flathub.org/login>, open the Developer Portal, the app, then **Verification**, and choose the website method. It shows the app's token, which never changes.
2. Publish it at **`https://waveflow.app/.well-known/org.flathub.VerifiedApps.txt`**, in the `waveflow-landing` repository (Cloudflare Pages). Plain text, one token per line, `#` lines ignored:

   ```text
   # app.waveflow.WaveFlow
   <token from the Verification page>
   ```

   HTTPS is required; a redirect is followed. Check that the URL answers the file as it is (not the site's 404 page), then press **Verify** on the portal.

(The portal also offers a DNS TXT record at `_flathub.waveflow.app`.)

## Every release after that

Each version is one PR to `flathub/app.waveflow.WaveFlow` (branch from `master`, PR against `master`):

1. The release entry in the metainfo and fresh generated sources, before the tag, as above.
2. In the Flathub repository: `tag` and `commit` set to the new tag, `cargo-sources.json` and `node-sources.json` copied again from that tag (`git show vX.Y.Z:packaging/flatpak/generated/...`), and the manifest copied again from `packaging/flatpak/flathub/` if it changed since the last release.
3. Flathub's bot builds the PR; merging it publishes the update to every user within hours.

A pre-release never goes there: Flathub's stable repository takes stable releases only.

# Upstream blockers — Tauri ecosystem debt

Status snapshot — refresh the dates whenever an issue moves.

WaveFlow ships on Tauri 2 + wry + tao + tauri-bundler. Some long-standing upstream limitations affect our UX or packaging. This doc tracks them so we don't keep rediscovering the same wall, and so future-us has a defensible policy on **when to wait, patch out-of-tree, PR upstream, or walk away**.

The lazy take "let's fork the whole stack" is almost always the wrong answer — see [Strategy](#strategy) below.

## Active blockers

### B1 — webkit2gtk-4.1 / GTK3 chrome on Linux

- **Upstream**: [tauri-apps/tauri#3961](https://github.com/tauri-apps/tauri/issues/3961) (open since 2022).
- **Impact**: Linux window decorations + native dialogs render in GTK3 style, which looks dated next to libadwaita apps on GNOME 49+. Affects every distribution channel (AUR / COPR / Flatpak / .deb / .rpm / AppImage), not Flatpak-specific.
- **Severity**: Cosmetic. The app is functional; only the chrome looks off.
- **Why it's stuck**: wry's Linux backend hard-binds against the GTK3 variant of WebKitGTK. The `webkit2gtk-rs` crate now exposes both 4.1 and 6.0 APIs behind features, so the missing piece is a wry refactor that conditionally compiles against either binding (~500–1500 lines). No PR has landed yet.
- **What we can do today**: ship a custom titlebar (`decorations: false` in `tauri.conf.json` + draw it ourselves) — Spotify / VS Code / Apple Music pattern. Cross-platform side benefit. Tracked under follow-ups in PR #164.
- **What a real fix looks like**: write the wry PR ourselves or sponsor it. Triage: when one of us has a focused two-week slot.

### B2 — AppImage sidecars

- **Upstream**: [tauri-apps/tauri#2667](https://github.com/tauri-apps/tauri/issues/2667), [tauri-apps/tauri-bundler issues](https://github.com/tauri-apps/tauri/labels/scope%3A%20bundler--appimage) (open since 2022).
- **Impact**: external binaries declared via `bundle.externalBin` don't resolve cleanly inside an AppImage's squashfs mount. Affects anyone shipping ffmpeg / yt-dlp / similar as a sidecar.
- **Severity**: **N/A for WaveFlow today** — we ship zero sidecars. The audio stack (symphonia + cpal) and external API clients (reqwest) are pure Rust. Logged here so we remember this constraint if we ever consider adding one.
- **Workaround if it ever bites us**: prefer the `.deb` / `.rpm` / Flatpak channels (which all handle sidecars correctly), drop AppImage entirely, or invoke via `$PATH` and document the system dep.

### B3 — Auto-updater vs. package managers

- **Status**: resolved on our side; documented here as the canonical pattern for future channels.
- **Impact**: every distribution channel that owns its own update flow (Flatpak, MS Store, MAS) fights with `tauri-plugin-updater` over the read-only install root.
- **Our fix**: `tauri-plugin-updater` is gated behind a default-on Cargo feature (`updater`). Channels that own updates compile with `--no-default-features`. The capability file `src-tauri/capabilities/updater.json` is generated/removed by `src-tauri/build.rs` based on the feature flag so `tauri-build`'s permission validation stays in sync.

## Strategy

Decision tree when an upstream issue blocks us:

1. **Is it cosmetic and does the app still work?** → Document, ship, move on. Don't burn a sprint on chrome. (B1.)
2. **Does it affect a channel we don't actually use yet?** → Same. (B2.)
3. **Does it block a release?**
   - **Trivial patch (< 100 lines)** → out-of-tree via `[patch.crates-io]` in `src-tauri/Cargo.toml`. We already do this for `glib = { path = "vendor/glib-0.18.5" }` (RUSTSEC-2024-0429 backport). Pattern: vendor the crate at the pinned version, apply the minimal diff, document in the crate's `PATCHES.md`. Drop the patch when upstream catches up.
   - **Non-trivial fix (100–1500 lines)** → write the PR upstream. Maintainers are responsive to concrete, tested PRs; what they don't do is prioritize work nobody is writing. Sponsoring an outside contributor is also valid if our time is the bottleneck.
   - **Architectural rewrite needed** → discuss before committing. Maintaining an out-of-tree fork of a moving ~150 kLOC subtree (`tauri` + `wry` + `tao` + `tauri-bundler`) is a 6-month tax that grows monthly. The cemetery of dead Electron / Tauri forks (Glimmr, Neutron-Native, …) is the lesson.
4. **Is the upstream project effectively unmaintained on this surface?** → That's the only honest case for a fork, and even then prefer forking the smallest possible subtree (a single crate, a single bundler module).

## Patch protocol

When B-level work justifies a `[patch.crates-io]` entry:

1. `git clone` the upstream crate at the version we depend on into `src-tauri/vendor/<crate>-<version>/`.
2. Apply the minimal diff. Commit with `vendor(<crate>): backport <upstream-pr-or-cve>`.
3. Add a `PATCHES.md` next to the crate root describing **what changed and why**, with links to the upstream PR / issue / CVE. Reviewers should be able to audit our delta against upstream in 60 seconds.
4. Wire the override in `src-tauri/Cargo.toml`:

   ```toml
   [patch.crates-io]
   <crate> = { path = "vendor/<crate>-<version>" }
   ```

5. Open an upstream PR with the same diff if it isn't already in flight. The patch is meant to be temporary.
6. Re-check at each Tauri minor bump: when upstream catches up, delete the vendor dir + patch entry, bump the dep, done.

## Maintenance

- Re-validate this doc at every Tauri minor bump.
- New blocker? Add a `B<n>` section above. Include the upstream issue link, severity, our workaround, and the realistic path to a real fix.
- Resolved blocker? Don't delete the section — move it to a short "Resolved" log at the bottom so we remember which PRs landed.

## Resolved

- _(nothing yet — placeholder)_

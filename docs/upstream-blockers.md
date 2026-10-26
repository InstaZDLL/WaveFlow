# Upstream blockers — Tauri ecosystem debt

Status snapshot — refresh the dates whenever an issue moves.

WaveFlow ships on Tauri 2 + wry + tao + tauri-bundler. Some long-standing upstream limitations affect our UX or packaging. This doc tracks them so we don't keep rediscovering the same wall, and so future-us has a defensible policy on **when to wait, patch out-of-tree, PR upstream, or walk away**.

The lazy take "let's fork the whole stack" is almost always the wrong answer — see [Strategy](#strategy) below.

## Active blockers

### B1 — webkit2gtk-4.1 / GTK3 chrome on Linux

- **Upstream**: [tauri-apps/tauri#3961](https://github.com/tauri-apps/tauri/issues/3961) (closed in 2022 in favour of the Flatpak route); the port itself is [tauri-apps/wry#1767](https://github.com/tauri-apps/wry/pull/1767) — gtk4 0.11 / webkit6, a community PR of about 8,400 added lines, open and unmerged as of October 2026 — with [tauri-apps/tao#1051](https://github.com/tauri-apps/tao/issues/1051) for the windowing half.
- **Impact**: Linux window decorations + native dialogs render in GTK3 style, which looks dated next to libadwaita apps on GNOME 49+. Affects every distribution channel (AUR / COPR / Flatpak / .deb / .rpm / AppImage), not Flatpak-specific.
- **Severity**: Cosmetic. The app is functional; only the chrome looks off.
- **Why it's stuck**: wry's Linux backend hard-binds against the GTK3 variant of WebKitGTK, and tao builds its windows with GTK3 — WaveFlow links tao 0.37.1, wry 0.57.0 and gtk 0.18 (GTK3). Moving means both crates at once; the wry PR above is the closest attempt and has not landed.
- **What we can do today**: ✅ **done, as a choice rather than a default** (#696, then the GNOME style). **Settings → Appearance → Windows → Window frame** offers, on a Wayland session, the desktop's frame restyled as a libadwaita header bar ([`adwaita_frame.rs`](../src-tauri/crates/app/src/adwaita_frame.rs): 46 px, round buttons, the theme's colours, while GTK keeps the button order, window menu, resize edges and shadow), and everywhere on Linux a frame WaveFlow draws (`set_decorations(false)` + [`AppTitleBar`](../src/components/layout/AppTitleBar.tsx)). A choice, because the GTK3 frame is the _right_ answer on a GTK desktop with its own theme and only looks dated next to libadwaita; and the same switch gives macOS the overlay title bar. The file pickers are the desktop's own, through its portal — KDE's on Plasma, the current GNOME one on GNOME ([`ui.md`](features/ui.md#file-pickers)); message dialogs stay GTK3. See [`ui.md`](features/ui.md#window-frame).
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

<!--
Thanks for the PR! A few quick reminders so review goes fast:

1. PR title follows Conventional Commits with a kebab-case scope:
     feat(audio): new shuffle mode
     fix(library): rescan skips broken symlinks
     perf(scanner): parallel BLAKE3 hashing
   Scopes mirror the auto-labeler rules in .github/labeler.yml.

2. Before opening, run locally:
     bun run lint
     bun run typecheck
     cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
     cargo check --manifest-path src-tauri/Cargo.toml --all-targets
   cargo fmt is the first step of the Rust CI job: one unformatted file
   fails the whole job, and cargo check does not warn about it.

3. If you touched user-facing strings, update every `src/i18n/locales/*.json`
   (17 locales — fr is the source of truth). No per-key fallback exists.

4. If you touched any cross-cutting pattern, update CLAUDE.md and / or the
   relevant docs/features/*.md so future readers (humans and Claude) stay
   in sync with the codebase.
-->

## Summary

<!-- 1-3 bullets describing what changes and why. Focus on the why. -->

-
-

## How I tested

<!-- Concrete steps a reviewer could repeat. Skip "it builds" — that's CI's job. -->

-
-

## Screenshots / clips

<!-- For UI changes. Drag-and-drop directly into the PR description on the web UI. -->

## Checklist

- [ ] Title uses Conventional Commits (`type(scope): subject`, kebab-case scope)
- [ ] `bun run lint` + `bun run typecheck` pass locally
- [ ] `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` passes locally
- [ ] `cargo check --manifest-path src-tauri/Cargo.toml --all-targets` passes locally
- [ ] If UI strings changed: every locale in `src/i18n/locales/` updated
- [ ] If cross-cutting pattern changed: `CLAUDE.md` and `docs/` updated
- [ ] Breaking change? Called out in the summary above

## Linked issues

<!-- Use "Closes #123" / "Refs #456" so GitHub auto-closes on merge.
     One keyword per issue: "Closes #123, closes #124". "Closes #123, #124"
     only closes #123. -->

Closes #

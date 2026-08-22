# Contributing to WaveFlow

Thanks for wanting to help. WaveFlow is a local music player built with **Tauri 2 + React 19 + TypeScript** on a **Rust** audio engine, using the **bun** toolchain. This file is the canonical contributing guide for the WaveFlow family — the satellite repos (server, Android, iOS) link back here and add their own specifics on top.

By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

## Getting set up

```bash
bun install                # dependencies
bun run tauri dev          # run the desktop app (Vite + Rust backend)
```

Frontend-only and backend-only loops:

```bash
bun run dev                # Vite dev server, no Tauri shell
bun run typecheck          # tsc --noEmit
bun run lint               # eslint
cargo check --manifest-path src-tauri/Cargo.toml --workspace --all-targets
cargo test  --manifest-path src-tauri/Cargo.toml --workspace
```

## Before you open a PR

Run the check — CI runs the same commands, so save yourself a round-trip:

```bash
bun run typecheck
bun run lint
cargo fmt --manifest-path src-tauri/Cargo.toml --all
cargo clippy --manifest-path src-tauri/Cargo.toml --workspace --all-targets -- -D warnings
cargo check --manifest-path src-tauri/Cargo.toml --workspace --all-targets
```

If you bump the pinned compiler, change `rust-toolchain.toml` **and** the
`toolchain:` input of every workflow that installs Rust, then run
`python3 scripts/check-toolchain-pin.py` — it is what CI runs, and it is
there because missing one of the six sites otherwise fails silently.

`rust-toolchain.toml` decides which compiler those run under, so a local
answer and the CI answer are the same answer. Let rustup install it rather
than reaching for your default toolchain.

If you touched a cross-cutting pattern (a context, the audio pipeline, a
migration, a sync wire shape), **update the docs in the same PR** — `CLAUDE.md`
and the relevant page under `docs/features/` are the source of truth and are
expected to stay in sync with the code.

## Commit conventions

[Conventional Commits](https://www.conventionalcommits.org/) are enforced
locally by a husky `commit-msg` hook (`bunx commitlint`). The rules that bite:

- `type(scope): subject` — e.g. `feat(player): gapless playback`.
- Scopes are **kebab-case** and mirror the areas in `.github/labeler.yml`.
- The subject stays **lowercase** — not sentence-case, start-case, or PascalCase.
- Header ≤ 100 characters.

Examples:

- `feat(scanner): split multi-artist tags on "; "`
- `fix(audio): clamp buffers to unity before the ring`
- `perf(artwork): cache covers per album instead of per song`
- `docs(playback): document the DoP idle-frame contract`

## Pull requests

- Keep a PR focused on one thing; smaller PRs get reviewed faster.
- The PR title also follows Conventional Commits — it drives the `type:` label
  and, with the diff size, the `size:` label.
- Fill in the PR template: what changed, why, and how you tested it.
- Link issues with `Closes #123` / `Refs #456` so they auto-close on merge.

## Translations

UI copy ships in **17 locales** (`src/i18n/locales/<code>.json`), with **`fr`**
as the source of truth and every locale carrying every key. When you add a key,
propagate it to all locales and leave brand tokens (`WaveFlow`, `Deezer`,
`Last.fm`, `ReplayGain`, `BPM`…) and `{{placeholder}}` tokens untouched.

## Reporting bugs and security issues

- Bugs and feature requests: use the [issue templates](.github/ISSUE_TEMPLATE/).
- Security vulnerabilities: **do not** open a public issue — follow
  [SECURITY.md](.github/SECURITY.md).

## License

WaveFlow is **GPL-3.0-only**. By submitting a pull request you agree that your
contribution is licensed under those terms for inclusion in this repository.

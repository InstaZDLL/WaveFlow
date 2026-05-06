# Repository Guidelines

## Project Structure & Module Organization

WaveFlow is a Tauri 2 desktop music player with a React/Vite frontend and Rust backend. Frontend code lives in `src/`: reusable UI in `components/common/`, layout shells in `components/layout/`, player controls in `components/player/`, route-level screens in `components/views/`, hooks in `hooks/`, contexts in `contexts/`, typed Tauri wrappers in `lib/tauri/`, and translations in `i18n/locales/`. Rust code lives in `src-tauri/src/`, with Tauri commands under `commands/`, audio engine code under `audio/`, database helpers under `db/`, and SQLite migrations in `src-tauri/migrations/{app,profile}/`. Static branding assets are in `assets/`; broader docs are in `docs/`.

## Build, Test, and Development Commands

Use Bun for JavaScript dependencies and scripts.

- `bun install` installs frontend tooling and Husky hooks.
- `bun run dev` starts the Vite frontend only.
- `bun run tauri dev` runs the full desktop app.
- `bun run build` runs TypeScript compilation and creates the Vite production build.
- `bun run lint` / `bun run lint:fix` check or fix TypeScript/React lint issues.
- `bun run typecheck` runs `tsc --noEmit`.
- `bun run format:check` verifies Prettier formatting.
- `cargo check --manifest-path src-tauri/Cargo.toml --all-targets` checks Rust backend code.
- `cargo test --manifest-path src-tauri/Cargo.toml` runs Rust tests.

## Coding Style & Naming Conventions

Frontend formatting is Prettier-controlled: 2 spaces, double quotes, semicolons, trailing commas, LF endings, and 120-column print width. React components use PascalCase, hooks use `useName`, and typed Tauri bridge files should stay grouped by domain in `src/lib/tauri/`. Rust should follow `rustfmt` defaults. Keep real-time audio callback code allocation-free, lock-free, and log-free.

## Testing Guidelines

There is no dedicated frontend test runner configured yet, so validate frontend changes with `bun run lint`, `bun run typecheck`, and targeted manual testing in `bun run tauri dev`. For backend changes, run `cargo check` and `cargo test`. If schema changes are needed, add a new timestamped migration; do not edit existing migrations.

## Commit & Pull Request Guidelines

Commits follow Conventional Commits enforced by Commitlint, with headers capped at 100 characters and kebab-case scopes. Examples: `fix(player): preserve queue order`, `docs: update release notes`, `feat(library): add album filter`.

Pull requests should be focused on one topic and include a summary, verification commands run, linked issue when applicable, migration notes for database changes, and screenshots for visible UI changes. Do not commit generated outputs such as `dist/`, `node_modules/`, or `src-tauri/target/`, and never include secrets, local databases, keys, or user music data.

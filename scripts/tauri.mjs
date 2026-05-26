#!/usr/bin/env node
// Wrapper around `tauri` that injects `--config <path>` after the
// subcommand. The Tauri CLI looks for `tauri.conf.json` next to the
// Cargo.toml of the binary it builds; after the Phase 1.a workspace
// split (RFC-001) that file lives at
// `src-tauri/crates/app/tauri.conf.json`, which the CLI cannot
// auto-discover from the repo root.
//
// This wrapper keeps the existing `bun run tauri <cmd>` ergonomics
// intact for both contributors and CI. It is a no-op for invocations
// that have no subcommand (e.g. `bun run tauri --version`).

import { spawn } from 'node:child_process';

const CONFIG_PATH = 'src-tauri/crates/app/tauri.conf.json';

// `--config` is a per-subcommand flag on the Tauri CLI, not a global
// one, and not every subcommand accepts it. Empirically (Tauri CLI
// 2.11) only the subcommands that actually load `tauri.conf.json`
// take the flag: `dev`, `build`, `bundle`. Everything else (info,
// icon, signer, completions, …) is passed through unchanged.
const CONFIG_AWARE = new Set(['dev', 'build', 'bundle']);

const argv = process.argv.slice(2);
const [subcommand] = argv;
const args =
  subcommand && CONFIG_AWARE.has(subcommand)
    ? [subcommand, '--config', CONFIG_PATH, ...argv.slice(1)]
    : argv;

const child = spawn('tauri', args, { stdio: 'inherit' });
child.on('error', (err) => {
  console.error(`failed to spawn tauri: ${err.message}`);
  process.exit(1);
});
child.on('exit', (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 1);
  }
});

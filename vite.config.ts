import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { readFileSync } from "node:fs";
import { fileURLToPath, URL } from "node:url";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

const root = (path: string) => fileURLToPath(new URL(path, import.meta.url));

/** The version of each package the frontend is built with, as installed. */
function npmVersion(name: string): string {
  const manifest = JSON.parse(
    readFileSync(root(`./node_modules/${name}/package.json`), "utf8"),
  ) as { version?: string };
  if (!manifest.version) throw new Error(`no version for npm package ${name}`);
  return manifest.version;
}

/** Orders two dotted versions by their numeric parts. */
function compareVersions(a: string, b: string): number {
  const pa = a.split(/[.+-]/).map((n) => Number.parseInt(n, 10) || 0);
  const pb = b.split(/[.+-]/).map((n) => Number.parseInt(n, 10) || 0);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (d !== 0) return d;
  }
  return 0;
}

/**
 * The version Cargo.lock pins for each crate. A crate locked at several
 * versions (a dependency still on an older major) is shown at the newest,
 * the one the app itself asks for.
 */
function crateVersions(names: string[]): Record<string, string> {
  const lock = readFileSync(root("./src-tauri/Cargo.lock"), "utf8");
  const found: Record<string, string> = {};
  for (const block of lock.split("[[package]]")) {
    const name = /^name = "([^"]+)"$/m.exec(block)?.[1];
    const version = /^version = "([^"]+)"$/m.exec(block)?.[1];
    if (!name || !version || !names.includes(name)) continue;
    const known = found[name];
    if (!known || compareVersions(version, known) > 0) found[name] = version;
  }
  for (const name of names) {
    if (!found[name]) throw new Error(`crate ${name} is not in Cargo.lock`);
  }
  return found;
}

/** The Rust edition the app crate is written in. */
function rustEdition(): string {
  const manifest = readFileSync(
    root("./src-tauri/crates/app/Cargo.toml"),
    "utf8",
  );
  const edition = /^edition = "([^"]+)"$/m.exec(manifest)?.[1];
  if (!edition) throw new Error("no edition in the app crate's Cargo.toml");
  return edition;
}

/**
 * The versions the About page lists, read from the lockfiles at build time
 * so the page never shows a version the build does not ship. A package
 * that goes missing fails the build rather than leaving a stale or blank
 * line. SQLite is asked of the linked library at runtime instead
 * (`get_sqlite_version`).
 */
function dependencyVersions() {
  const crates = crateVersions([
    "tauri",
    "sqlx",
    "tokio",
    "symphonia",
    "cpal",
    "rubato",
  ]);
  return {
    tauri: crates.tauri,
    react: npmVersion("react"),
    vite: npmVersion("vite"),
    typescript: npmVersion("typescript"),
    tailwindcss: npmVersion("tailwindcss"),
    rustEdition: rustEdition(),
    sqlx: crates.sqlx,
    tokio: crates.tokio,
    symphonia: crates.symphonia,
    cpal: crates.cpal,
    rubato: crates.rubato,
  };
}

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react(), tailwindcss()],

  define: {
    __DEPENDENCY_VERSIONS__: JSON.stringify(dependencyVersions()),
  },

  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },

  build: {
    rolldownOptions: {
      output: {
        manualChunks(id) {
          if (
            id.includes("/node_modules/react/") ||
            id.includes("/node_modules/react-dom/")
          ) {
            return "vendor-react";
          }
          if (
            id.includes("/node_modules/i18next/") ||
            id.includes("/node_modules/i18next-browser-languagedetector/") ||
            id.includes("/node_modules/react-i18next/")
          ) {
            return "vendor-i18n";
          }
          if (
            id.includes("/node_modules/@tauri-apps/api/") ||
            id.includes("/node_modules/@tauri-apps/plugin-dialog/") ||
            id.includes("/node_modules/@tauri-apps/plugin-opener/") ||
            id.includes("/node_modules/@tauri-apps/plugin-updater/")
          ) {
            return "vendor-tauri";
          }
          if (
            id.includes("/node_modules/lucide-react/") ||
            id.includes("/node_modules/@dnd-kit/") ||
            id.includes("/node_modules/@tanstack/react-virtual/")
          ) {
            return "vendor-ui";
          }
        },
      },
    },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));

import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath, URL } from "node:url";
import { createReadStream, existsSync, statSync } from "node:fs";
import { extname, join, normalize, sep } from "node:path";
import type { Plugin } from "vite";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

/**
 * Serves `prototypes/fluid-editorial/` at `/fluid-proto/` during
 * `vite dev` only. Design prototypes (`?proto=<name>`, see `src/main.tsx`)
 * draw on the user's own album art, which must neither enter the
 * repository nor a build: `prototypes/` is gitignored and sits outside
 * `public/`, so `vite build` never copies it. Nothing here runs in
 * production.
 */
function prototypeAssets(): Plugin {
  const root = fileURLToPath(
    new URL("./prototypes/fluid-editorial", import.meta.url),
  );
  const types: Record<string, string> = {
    ".jpg": "image/jpeg",
    ".jpeg": "image/jpeg",
    ".png": "image/png",
    ".webp": "image/webp",
  };
  return {
    name: "waveflow:prototype-assets",
    apply: "serve",
    configureServer(server) {
      server.middlewares.use("/fluid-proto", (req, res, next) => {
        const rel = decodeURIComponent((req.url ?? "/").split("?")[0]);
        const file = normalize(join(root, rel));
        // `root + sep`, not `root`: a sibling folder sharing the prefix
        // (`fluid-editorial-private/`) would otherwise pass the check.
        if (
          !file.startsWith(root + sep) ||
          !existsSync(file) ||
          !statSync(file).isFile()
        ) {
          next();
          return;
        }
        res.setHeader(
          "Content-Type",
          types[extname(file)] ?? "application/octet-stream",
        );
        res.setHeader("Cache-Control", "no-cache");
        createReadStream(file).pipe(res);
      });
    },
  };
}

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react(), tailwindcss(), prototypeAssets()],

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

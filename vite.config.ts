import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath, URL } from "node:url";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react(), tailwindcss()],

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

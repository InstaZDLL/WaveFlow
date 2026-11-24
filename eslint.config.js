import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import globals from "globals";

export default tseslint.config(
  {
    ignores: [
      "**/node_modules/**",
      "**/dist/**",
      "**/target/**",
      "src-tauri/**",
      "waveflow-landing/**",
      "vscode-discord-rich-presence/**",
      "scripts/**",
      "secrets/**",
      "packaging/**",
      // AI agent skills / tooling — not application code (Node globals,
      // hook-shaped helpers) and not ours to lint.
      ".claude/**",
      ".agents/**",
      "**/*.config.ts",
      "**/*.config.js",
      "**/.commitlintrc.cjs",
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    languageOptions: {
      globals: globals.browser,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-refresh/only-export-components": [
        "warn",
        { allowConstantExport: true },
      ],
      // A command's error that carries a kind reaches the UI as a
      // `BackendError` only through the wrapper (#1157).
      "no-restricted-imports": [
        "error",
        {
          paths: [
            {
              name: "@tauri-apps/api/core",
              importNames: ["invoke"],
              message:
                "Import invoke from src/lib/tauri/invoke.ts, which turns an error with a kind into a BackendError.",
            },
          ],
        },
      ],
    },
  },
  {
    files: ["src/lib/tauri/invoke.ts"],
    rules: {
      "no-restricted-imports": "off",
    },
  },
  {
    rules: {
      "@typescript-eslint/no-unused-vars": [
        "warn",
        { argsIgnorePattern: "^_" },
      ],
      "@typescript-eslint/no-explicit-any": "warn",
    },
  },
);

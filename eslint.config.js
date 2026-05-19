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
      "**/*.config.ts",
      "**/*.config.js",
      "**/.commitlintrc.cjs",
    ],
  },
  {
    // A few `// eslint-disable-next-line` directives in `src/components/`
    // target jsx-a11y rules that Codacy runs server-side but that we
    // don't load in this local config (no `eslint-plugin-jsx-a11y`
    // dependency). Without this opt-out, eslint v9 flags them as
    // "Unused eslint-disable directive" locally even though they're
    // load-bearing for the Codacy run.
    linterOptions: {
      reportUnusedDisableDirectives: "off",
    },
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

/// <reference types="vite/client" />

/** The versions the About page lists, read from the lockfiles at build
 *  time (`dependencyVersions` in `vite.config.ts`). */
declare const __DEPENDENCY_VERSIONS__: {
  tauri: string;
  react: string;
  vite: string;
  typescript: string;
  tailwindcss: string;
  rustEdition: string;
  sqlx: string;
  tokio: string;
  symphonia: string;
  cpal: string;
  rubato: string;
};

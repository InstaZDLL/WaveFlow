import { lazy } from "react";

/**
 * Entry for `main.tsx`. `import.meta.env.DEV` is a build-time constant,
 * so in a production build this is `null` and the dynamic import — the
 * whole prototype chunk — is dead code the bundler drops.
 */
export const FluidEditorialPrototype = import.meta.env.DEV
  ? lazy(() => import("./FluidEditorialApp"))
  : null;

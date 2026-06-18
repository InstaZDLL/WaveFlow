/**
 * Skin presets — the layer above [`ThemePreset`](./themes.ts).
 *
 * **Themes** control the accent palette (emerald → indigo, sunset, …)
 * and the surface tint family. They don't touch shape, density,
 * typography or motion.
 *
 * **Skins** control the *language* of the UI: how dense it breathes,
 * what kind of materials its surfaces evoke, what typeface carries
 * the headings, how transitions feel. A skin can fundamentally
 * change the personality of the app the way a font swap or a
 * spacing-scale rewrite does in a real design system.
 *
 * The two axes are orthogonal so any (theme, skin) pair works.
 * A skin swap re-styles every surface that opts into the skin
 * tokens through Tailwind v4's `@theme inline` indirection (see
 * `app.css`) — no per-component branching required.
 *
 * Adding a new skin is a one-stop process: declare the tokens
 * here and the Settings → Appearance picker exposes it
 * automatically. Components that want to opt into a token simply
 * use the corresponding Tailwind utility (`rounded-card`,
 * `font-display`, `shadow-elevated`, etc.) instead of the
 * baseline equivalents.
 */

export type SkinId = "studio" | "editorial" | "lounge" | "pulse";

export interface SkinPreset {
  /** Stable id used as the persisted value and the `data-skin` attribute. */
  id: SkinId;
  /** i18n key suffix, e.g. `appearance.skins.editorial.label`. */
  labelKey: string;
  /**
   * One-sentence i18n description shown under the radio in the
   * picker — gives the user a feel for the mood before they
   * commit.
   */
  descriptionKey: string;
  /**
   * Density tokens — spacing-scale multipliers used by the
   * Sidebar / TopBar / PlayerBar / list rows. Values are
   * unitless and multiply Tailwind's default 0.25-rem scale
   * inside the `@theme inline` block.
   *
   * Convention: `1.0` = current Studio density. Editorial sits
   * around `1.35-1.5` for a magazine-style airier feel.
   */
  density: {
    /** Sidebar nav row vertical padding. */
    navRow: number;
    /** TopBar height. */
    topBar: number;
    /** TrackTable row height. */
    listRow: number;
    /** Generic card / section padding. */
    cardPad: number;
  };
  /**
   * Radius tokens (in CSS `px` or `rem` strings). Skin chooses
   * whether the UI reads as Apple-style pillows (1rem), magazine
   * paper (0.125rem), or pill-shaped neon chips (9999px).
   */
  radius: {
    card: string;
    button: string;
    input: string;
    pill: string;
  };
  /**
   * Surface material tokens. Editorial reads as ink-on-paper
   * (no shadows, no blur, optional grain). Studio is the
   * Apple-Music-ish soft-shadow baseline. Future Pulse / Lounge
   * skins will pile on the heavier-handed materials (neon
   * glows, glass blurs).
   */
  surface: {
    /** Card-level shadow CSS value (e.g. `none`, `0 1px 2px …`). */
    shadowCard: string;
    /** Elevated panel shadow (PlayerBar, modals). */
    shadowElevated: string;
    /** Backdrop-filter value for glass panels (`none` for non-blur). */
    backdrop: string;
    /** Hairline divider color override — Editorial overrides to a
     *  warm sepia ink line, Studio keeps the zinc-200 default. */
    divider: string;
    /**
     * Subtle texture overlay (e.g. paper grain). Empty string
     * disables. Editorial sets a low-opacity SVG noise; Studio
     * leaves it blank for flat surfaces.
     */
    grain: string;
  };
  /**
   * Typography tokens. The hero swap most users will feel.
   * Editorial swaps the entire display family to a serif so
   * sidebar / topbar headings carry a magazine feel; Studio
   * sticks with the system sans baseline.
   */
  typography: {
    /** Font-family stack used by display / heading surfaces. */
    display: string;
    /** Font-family stack used by body / interactive elements. */
    body: string;
    /** Heading font-weight (Editorial leans light, Studio semibold). */
    headingWeight: number;
    /** Display letter-spacing — Editorial likes a touch more air. */
    displayTracking: string;
  };
  /**
   * Motion tokens — translation of the skin's mood into Framer
   * Motion-friendly numbers. Read by `MotionConfig` at the
   * application root so every animated subtree picks up the
   * skin's pace without component-level conditionals.
   */
  motion: {
    /** Generic transition duration in seconds. */
    duration: number;
    /** Spring stiffness for the "snap" presets. */
    springStiffness: number;
    /** Spring damping for the "snap" presets. */
    springDamping: number;
    /** CSS `easing-function` token for non-spring transitions. */
    ease: string;
  };
}

/** Stable id of the skin shipped before this module landed. */
export const DEFAULT_SKIN_ID: SkinId = "studio";

export const SKIN_PRESETS: SkinPreset[] = [
  {
    id: "studio",
    labelKey: "settings.appearance.skins.studio.label",
    descriptionKey: "settings.appearance.skins.studio.description",
    density: {
      navRow: 1.0,
      topBar: 1.0,
      listRow: 1.0,
      cardPad: 1.0,
    },
    radius: {
      card: "0.75rem",
      button: "0.75rem",
      input: "0.5rem",
      pill: "9999px",
    },
    surface: {
      shadowCard: "0 1px 2px 0 rgb(0 0 0 / 0.05)",
      shadowElevated: "0 4px 12px -2px rgb(0 0 0 / 0.08)",
      backdrop: "none",
      // `currentColor` resolution: leaves the existing zinc-200 /
      // zinc-700 dark hairlines as-is — components using
      // `border-zinc-200 dark:border-zinc-700` keep working.
      // Components that opt into `border-divider` get this value.
      divider: "rgb(228 228 231)", // zinc-200
      grain: "",
    },
    typography: {
      display:
        '"Inter", system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif',
      body:
        '"Inter", system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif',
      headingWeight: 600,
      displayTracking: "-0.011em",
    },
    motion: {
      duration: 0.25,
      springStiffness: 320,
      springDamping: 28,
      ease: "cubic-bezier(0.16, 1, 0.3, 1)",
    },
  },
  {
    id: "editorial",
    labelKey: "settings.appearance.skins.editorial.label",
    descriptionKey: "settings.appearance.skins.editorial.description",
    density: {
      navRow: 1.4,
      topBar: 1.5,
      listRow: 1.35,
      cardPad: 1.5,
    },
    radius: {
      // Magazine / book chrome — paper has no rounded corners,
      // just deliberate hairlines.
      card: "0.125rem",
      button: "0.25rem",
      input: "0.125rem",
      pill: "0.125rem",
    },
    surface: {
      // No shadows — paper sits flat on the desk.
      shadowCard: "none",
      shadowElevated: "none",
      backdrop: "none",
      // Sepia-warm hairline that reads as printed-ink rather
      // than mechanical UI divider.
      divider: "rgb(168 162 158)", // stone-400
      // A subtle SVG noise overlay paints "paper grain" under
      // every surface that uses `bg-paper`. Generated inline
      // so we don't have to ship an asset — cheap on GPU
      // because the SVG noise is tiled at 200×200.
      grain:
        "url(\"data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='200' height='200'><filter id='n'><feTurbulence type='fractalNoise' baseFrequency='0.85' numOctaves='2' stitchTiles='stitch'/><feColorMatrix values='0 0 0 0 0.45 0 0 0 0 0.42 0 0 0 0 0.36 0 0 0 0.08 0'/></filter><rect width='200' height='200' filter='url(%23n)'/></svg>\")",
    },
    typography: {
      // Playfair Display: a high-contrast didone with theatrical
      // serifs — the canonical broadsheet masthead face. Lora as
      // the body face: a robust serif with strong italic stroke
      // for pull-quotes and figure captions. Both are loaded via
      // a `@import` at the top of `editorial.css` so the FOIT is
      // scoped to this skin (other skins don't pay).
      display:
        '"Playfair Display", "Source Serif Pro", Georgia, "Times New Roman", serif',
      body:
        '"Lora", Georgia, "Times New Roman", serif',
      // Editorial mastheads carry weight in the GLYPH design,
      // not in the font-weight token. Playfair at 900 reads as
      // a thick wood-type poster headline, which is the look we
      // want for "WAVEFLOW GAZETTE".
      headingWeight: 900,
      displayTracking: "-0.02em",
    },
    motion: {
      // Paper is slow. Reading is unhurried. A 300 ms ease-out
      // makes the UI feel like turning a page.
      duration: 0.32,
      springStiffness: 180,
      springDamping: 26,
      ease: "cubic-bezier(0.4, 0, 0.2, 1)",
    },
  },
  {
    id: "lounge",
    labelKey: "settings.appearance.skins.lounge.label",
    descriptionKey: "settings.appearance.skins.lounge.description",
    density: {
      // The hero of Lounge is breathing room — everything sits
      // further apart so the cover-art backdrop has space to
      // breathe through panels.
      navRow: 1.45,
      topBar: 1.4,
      listRow: 1.3,
      cardPad: 1.5,
    },
    radius: {
      // Soft, rounded pill-card territory. The radii reinforce
      // the "ambient hi-fi" mood — nothing sharp.
      card: "1.5rem",
      button: "1.5rem",
      input: "1rem",
      pill: "9999px",
    },
    surface: {
      // No flat shadows — glass diffuses light differently. The
      // illusion comes from the backdrop blur + a very subtle
      // inner-rim highlight via the elevated shadow.
      shadowCard: "0 8px 32px -8px rgb(0 0 0 / 0.18)",
      shadowElevated:
        "inset 0 1px 0 rgb(255 255 255 / 0.15), 0 12px 48px -12px rgb(0 0 0 / 0.35)",
      // Aggressive backdrop blur paints the glass-panel illusion
      // over whatever lives behind (the SkinAmbientBackdrop
      // component renders the current track's cover at full
      // viewport scale, blurred + dimmed).
      backdrop: "blur(28px) saturate(130%)",
      // Translucent ink-light divider so hairlines don't fight
      // the glass surfaces.
      divider: "rgb(255 255 255 / 0.10)",
      grain: "",
    },
    typography: {
      // Stay on the sans-serif baseline (Inter) — the skin's
      // personality is in the surface material, not the type.
      // Heavier display weight + tighter tracking lands the
      // "premium hi-fi" feel.
      display:
        '"Inter", system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif',
      body:
        '"Inter", system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif',
      headingWeight: 500,
      displayTracking: "-0.02em",
    },
    motion: {
      // Slow + smooth — every transition feels like a fade-in.
      // Springs are intentionally underdamped (softer) so cover
      // swaps glide instead of snapping.
      duration: 0.45,
      springStiffness: 140,
      springDamping: 22,
      ease: "cubic-bezier(0.22, 1, 0.36, 1)",
    },
  },
  {
    id: "pulse",
    labelKey: "settings.appearance.skins.pulse.label",
    descriptionKey: "settings.appearance.skins.pulse.description",
    density: {
      // Tight + punchy. Pulse is a clubby skin: list rows
      // compress closer together so the user gets more of the
      // catalogue in view at once.
      navRow: 0.9,
      topBar: 0.95,
      listRow: 0.85,
      cardPad: 0.9,
    },
    radius: {
      // Pills everywhere. Buttons, chips, even cards lean
      // heavily into the pill shape — reads as gaming/club
      // UI.
      card: "1rem",
      button: "9999px",
      input: "9999px",
      pill: "9999px",
    },
    surface: {
      // Neon glow shadows — these are what make Pulse feel
      // distinct. Each card carries a coloured halo via the
      // accent palette so the cover art / accent ring around
      // selected items pulses into the surrounding chrome.
      shadowCard:
        "0 0 0 1px rgb(255 255 255 / 0.06), 0 0 24px -4px var(--accent-500)",
      shadowElevated:
        "0 0 0 1px rgb(255 255 255 / 0.08), 0 0 48px -8px var(--accent-400)",
      backdrop: "none",
      // Subtle white divider — Pulse is dark-only so the lines
      // need to lift off the OLED base.
      divider: "rgb(255 255 255 / 0.08)",
      grain: "",
    },
    typography: {
      // Mono / tech display family. JetBrains Mono is in most
      // dev fonts caches; falls back to ui-monospace on macOS
      // and Consolas on Windows so no FOIT.
      display:
        '"JetBrains Mono", "Fira Code", ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace',
      body:
        '"Inter", system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif',
      headingWeight: 700,
      displayTracking: "0.04em",
    },
    motion: {
      // Punchy spring — short duration, stiff spring,
      // overdamped enough to feel bouncy without overshooting.
      // Pulse animations land.
      duration: 0.18,
      springStiffness: 480,
      springDamping: 22,
      ease: "cubic-bezier(0.34, 1.56, 0.64, 1)",
    },
  },
];

export function findSkin(id: string | null | undefined): SkinPreset {
  if (!id) return SKIN_PRESETS.find((s) => s.id === DEFAULT_SKIN_ID)!;
  return (
    SKIN_PRESETS.find((s) => s.id === id) ??
    SKIN_PRESETS.find((s) => s.id === DEFAULT_SKIN_ID)!
  );
}

/**
 * Apply the skin's CSS variables + `data-skin` attribute to the
 * document root. Mirrors [`applyTheme`](./themes.ts) — every
 * token is set on every call so swapping back to Studio clears
 * the previous skin's overrides cleanly.
 */
export function applySkin(skin: SkinPreset) {
  const root = document.documentElement;

  // Density — these go directly into Tailwind v4 `@theme inline`
  // bindings in app.css, so writing them retints every utility
  // that opts in (`p-nav`, `h-topbar`, …) without component
  // edits.
  root.style.setProperty("--skin-density-nav", `${skin.density.navRow}`);
  root.style.setProperty("--skin-density-topbar", `${skin.density.topBar}`);
  root.style.setProperty("--skin-density-list", `${skin.density.listRow}`);
  root.style.setProperty("--skin-density-card", `${skin.density.cardPad}`);

  // Radius
  root.style.setProperty("--skin-radius-card", skin.radius.card);
  root.style.setProperty("--skin-radius-button", skin.radius.button);
  root.style.setProperty("--skin-radius-input", skin.radius.input);
  root.style.setProperty("--skin-radius-pill", skin.radius.pill);

  // Surface
  root.style.setProperty("--skin-shadow-card", skin.surface.shadowCard);
  root.style.setProperty("--skin-shadow-elevated", skin.surface.shadowElevated);
  root.style.setProperty("--skin-backdrop", skin.surface.backdrop);
  root.style.setProperty("--skin-divider", skin.surface.divider);
  root.style.setProperty("--skin-grain", skin.surface.grain || "none");

  // Typography
  root.style.setProperty("--skin-font-display", skin.typography.display);
  root.style.setProperty("--skin-font-body", skin.typography.body);
  root.style.setProperty(
    "--skin-heading-weight",
    `${skin.typography.headingWeight}`,
  );
  root.style.setProperty(
    "--skin-display-tracking",
    skin.typography.displayTracking,
  );

  // Motion — read by MotionConfig at the application root.
  root.style.setProperty("--skin-motion-duration", `${skin.motion.duration}s`);
  root.style.setProperty(
    "--skin-motion-spring-stiffness",
    `${skin.motion.springStiffness}`,
  );
  root.style.setProperty(
    "--skin-motion-spring-damping",
    `${skin.motion.springDamping}`,
  );
  root.style.setProperty("--skin-motion-ease", skin.motion.ease);

  root.setAttribute("data-skin", skin.id);
}

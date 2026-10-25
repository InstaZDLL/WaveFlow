import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Pipette, Paintbrush } from "lucide-react";

import { useAccentChoice } from "../../../hooks/useAccentChoice";
import { useProfile } from "../../../hooks/useProfile";
import {
  ACCENT_SHADES,
  ACCENT_SWATCHES,
  scaleFromHex,
  type AccentChoice,
} from "../../../lib/accent";

type Mode = AccentChoice["mode"];
const MODES: Mode[] = ["theme", "custom", "cover"];

// Each swatch shows the 500 stop it would produce, worked out once.
const SWATCHES = ACCENT_SWATCHES.map((swatch) => ({
  ...swatch,
  fill: scaleFromHex(swatch.hex)[500],
}));
const DEFAULT_CUSTOM = ACCENT_SWATCHES[7].hex;

/**
 * Accent colour, laid over the active theme (2.0): the theme's own, a
 * picked colour, or the playing track's cover.
 *
 * The whole app is the preview — the choice applies as it is made — so
 * the card only shows the eleven stops in use, read from the applied
 * variables, which is where a pick that came out muddier than expected
 * becomes legible.
 */
export function AccentCard() {
  const { t } = useTranslation();
  const { value: choice, ready, setValue } = useAccentChoice();

  // Switching to "custom" starts from blue: the stored choice keeps no
  // colour while another mode is on.
  const customColor = choice.mode === "custom" ? choice.color : DEFAULT_CUSTOM;

  const pickMode = (mode: Mode) => {
    if (mode === choice.mode) return;
    void setValue(mode === "custom" ? { mode, color: customColor } : { mode });
  };
  const pickColor = (color: string) => {
    void setValue({ mode: "custom", color: color.toLowerCase() });
  };

  // Dragging in the native picker fires a change per step. The input
  // follows the drag at once; the choice is written once it settles.
  const [draft, setDraft] = useState<string | null>(null);
  const settle = useRef<number | null>(null);
  const pending = useRef<string | null>(null);
  const flush = () => {
    if (settle.current !== null) window.clearTimeout(settle.current);
    settle.current = null;
    const color = pending.current;
    pending.current = null;
    setDraft(null);
    if (color) pickColor(color);
  };
  const dragColor = (color: string) => {
    setDraft(color);
    pending.current = color;
    if (settle.current !== null) window.clearTimeout(settle.current);
    settle.current = window.setTimeout(flush, 200);
  };
  // A choice still pending when the card goes away is written, not lost.
  useEffect(() => {
    const timer = settle;
    const queued = pending;
    return () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
      const color = queued.current;
      queued.current = null;
      if (color) void setValue({ mode: "custom", color: color.toLowerCase() });
    };
  }, [setValue]);

  // A colour still settling belongs to the profile it was picked in: a
  // switch drops it rather than let the timer write it into the next one.
  // Declared after the effect above on purpose: on unmount React runs the
  // cleanups in declaration order, so the pending colour is written there
  // before this one would drop it.
  const { activeProfile } = useProfile();
  const profileId = activeProfile?.id ?? null;
  useEffect(() => {
    const timer = settle;
    const queued = pending;
    return () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
      timer.current = null;
      queued.current = null;
      setDraft(null);
    };
  }, [profileId]);

  return (
    <div className="settings-block">
      <div className="flex items-center space-x-4 min-w-0">
        <Paintbrush
          size={16}
          className="settings-row-icon"
          aria-hidden="true"
        />
        <div className="min-w-0">
          <p
            id="accent-card-title"
            className="text-sm font-medium text-zinc-800 dark:text-zinc-200"
          >
            {t("settings.appearance.accent.title")}
          </p>
          <p className="text-xs mt-0.5 settings-description">
            {t("settings.appearance.accent.subtitle")}
          </p>
        </div>
      </div>

      <div className="mt-4 ml-9 space-y-4">
        {/* Native radios, visually hidden behind their labels: arrow
            keys move between the modes, as in the other Settings
            choices, without a hand-rolled roving focus. */}
        <div
          role="radiogroup"
          aria-labelledby="accent-card-title"
          aria-busy={!ready}
          className="inline-flex rounded-xl border border-zinc-200 dark:border-zinc-700 p-1 gap-1"
        >
          {MODES.map((mode) => (
            <label key={mode} className="relative">
              <input
                type="radio"
                name="wf-accent-mode"
                value={mode}
                checked={ready && choice.mode === mode}
                // Inert until the profile's choice is read, so a click
                // cannot persist over a choice not loaded yet.
                disabled={!ready}
                onChange={() => pickMode(mode)}
                className="peer sr-only"
              />
              <span className="block px-3 py-1.5 rounded-lg text-xs font-medium cursor-pointer transition-colors text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800 peer-checked:bg-emerald-500 peer-checked:text-white peer-checked:hover:bg-emerald-500 peer-focus-visible:ring-2 peer-focus-visible:ring-emerald-500 peer-disabled:opacity-50 peer-disabled:cursor-default">
                {t(`settings.appearance.accent.mode.${mode}`)}
              </span>
            </label>
          ))}
        </div>

        {choice.mode === "custom" && (
          <div className="flex flex-wrap items-center gap-2">
            {SWATCHES.map((swatch) => {
              const selected = choice.color === swatch.hex;
              const name = t(
                `settings.appearance.accent.colors.${swatch.name}`,
              );
              return (
                <button
                  key={swatch.hex}
                  type="button"
                  onClick={() => pickColor(swatch.hex)}
                  aria-pressed={selected}
                  aria-label={name}
                  title={name}
                  className={`w-7 h-7 rounded-full flex items-center justify-center transition-transform motion-safe:hover:scale-110 focus:outline-none focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-zinc-400 dark:focus-visible:ring-offset-zinc-900 ${
                    selected
                      ? "ring-2 ring-offset-2 ring-zinc-900 dark:ring-white dark:ring-offset-zinc-900"
                      : ""
                  }`}
                  style={{ backgroundColor: swatch.fill }}
                >
                  {selected && (
                    <Check
                      size={14}
                      strokeWidth={3}
                      className="text-white"
                      aria-hidden="true"
                    />
                  )}
                </button>
              );
            })}
            {/* Any other colour. The native picker is the one people
                know, and it gives keyboard and screen-reader access for
                free. */}
            <label
              className={`relative w-7 h-7 rounded-full flex items-center justify-center border border-dashed border-zinc-300 dark:border-zinc-600 cursor-pointer hover:border-zinc-400 focus-within:ring-2 focus-within:ring-zinc-400 ${
                SWATCHES.some((swatch) => swatch.hex === choice.color)
                  ? ""
                  : "ring-2 ring-offset-2 ring-zinc-900 dark:ring-white dark:ring-offset-zinc-900"
              }`}
              title={t("settings.appearance.accent.other")}
            >
              <Pipette
                size={13}
                className="text-zinc-500 dark:text-zinc-400"
                aria-hidden="true"
              />
              <input
                type="color"
                value={draft ?? choice.color}
                onChange={(event) => dragColor(event.target.value)}
                aria-label={t("settings.appearance.accent.other")}
                className="absolute inset-0 opacity-0 cursor-pointer"
              />
            </label>
          </div>
        )}

        {choice.mode === "cover" && (
          <p className="text-xs settings-description">
            {t("settings.appearance.accent.coverNote")}
          </p>
        )}

        <div
          className="flex h-3 w-full max-w-xs overflow-hidden rounded-full"
          aria-hidden="true"
        >
          {ACCENT_SHADES.map((shade) => (
            <div
              key={shade}
              className="flex-1"
              // The applied variables, so the cover's scale shows too.
              style={{ backgroundColor: `var(--accent-${shade})` }}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Pipette, Paintbrush } from "lucide-react";

import { useAccentChoice } from "../../../hooks/useAccentChoice";
import {
  ACCENT_SHADES,
  ACCENT_SWATCHES,
  scaleFromHex,
  type AccentChoice,
} from "../../../lib/accent";

type Mode = AccentChoice["mode"];
const MODES: Mode[] = ["theme", "custom", "cover"];

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
  const customColor =
    choice.mode === "custom" ? choice.color : ACCENT_SWATCHES[7];

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

  return (
    <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
      <div className="flex items-center space-x-4 min-w-0">
        <Paintbrush
          size={20}
          className="shrink-0 text-zinc-500 dark:text-zinc-400"
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
        <div
          role="radiogroup"
          aria-labelledby="accent-card-title"
          className="inline-flex rounded-xl border border-zinc-200 dark:border-zinc-700 p-1 gap-1"
        >
          {MODES.map((mode) => {
            const active = ready && choice.mode === mode;
            return (
              <button
                key={mode}
                type="button"
                role="radio"
                aria-checked={active}
                disabled={!ready}
                onClick={() => pickMode(mode)}
                className={`px-3 py-1.5 rounded-lg text-xs font-medium transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 ${
                  active
                    ? "bg-emerald-500 text-white"
                    : "text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
                }`}
              >
                {t(`settings.appearance.accent.mode.${mode}`)}
              </button>
            );
          })}
        </div>

        {choice.mode === "custom" && (
          <div className="flex flex-wrap items-center gap-2">
            {ACCENT_SWATCHES.map((swatch) => {
              const selected = choice.color === swatch;
              return (
                <button
                  key={swatch}
                  type="button"
                  onClick={() => pickColor(swatch)}
                  aria-pressed={selected}
                  aria-label={t("settings.appearance.accent.swatch", {
                    color: swatch,
                  })}
                  title={swatch}
                  className={`w-7 h-7 rounded-full flex items-center justify-center transition-transform hover:scale-110 focus:outline-none focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-zinc-400 dark:focus-visible:ring-offset-zinc-900 ${
                    selected
                      ? "ring-2 ring-offset-2 ring-zinc-900 dark:ring-white dark:ring-offset-zinc-900"
                      : ""
                  }`}
                  style={{ backgroundColor: scaleFromHex(swatch)[500] }}
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
                (ACCENT_SWATCHES as readonly string[]).includes(choice.color)
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

import { useTranslation } from "react-i18next";
import { Check, Palette } from "lucide-react";
import {
  LYRICS_HIGHLIGHT_ORDER,
  LYRICS_PASTELS,
  useLyricsHighlightColor,
  type LyricsHighlightId,
} from "../../../hooks/useLyricsHighlightColor";

/** The rainbow swatch: the palette itself, walked round, since that is
 *  what the mode paints the lines with. */
const RAINBOW_SWATCH = `conic-gradient(from 0deg, ${[
  ...Object.values(LYRICS_PASTELS),
  LYRICS_PASTELS.rose,
].join(", ")})`;

type Choice = LyricsHighlightId | "default";

const CHOICES: Choice[] = ["default", ...LYRICS_HIGHLIGHT_ORDER];

function swatchBackground(choice: Choice): string {
  if (choice === "default") return "#ffffff";
  if (choice === "rainbow") return RAINBOW_SWATCH;
  return LYRICS_PASTELS[choice];
}

/**
 * Settings → Lyrics row for the colour of the line being sung in the
 * immersive lyrics and the mini-player (#751). The desktop lyrics window
 * has its own colours, in its own card, and keeps them.
 *
 * A row of swatches rather than a colour picker: the lines are drawn on
 * dark surfaces, and a free pick could not promise they stay readable.
 * Native radios under the swatches, so arrow keys move between them.
 */
export function LyricsHighlightColorCard() {
  const { t } = useTranslation();
  const { id, ready, setId } = useLyricsHighlightColor();
  const current: Choice = id ?? "default";

  return (
    <section
      aria-label={t("settings.lyricsHighlightColor.title")}
      className="px-4 py-3"
    >
      <div className="flex items-start gap-3">
        <Palette
          size={20}
          className="text-zinc-400 mt-0.5 shrink-0"
          aria-hidden="true"
        />
        <div className="min-w-0 flex-1">
          <span
            className="block text-sm font-medium text-zinc-900 dark:text-white"
            id="settings-lyrics-highlight-label"
          >
            {t("settings.lyricsHighlightColor.title")}
          </span>
          <span className="block text-xs mt-0.5 settings-description">
            {t("settings.lyricsHighlightColor.subtitle")}
          </span>
          <div
            role="radiogroup"
            aria-labelledby="settings-lyrics-highlight-label"
            aria-busy={!ready}
            className={`mt-3 flex flex-wrap gap-2.5 ${ready ? "" : "opacity-50"}`}
          >
            {CHOICES.map((choice) => {
              const label = t(`settings.lyricsHighlightColor.colors.${choice}`);
              const checked = current === choice;
              return (
                <label
                  key={choice}
                  title={label}
                  className="relative cursor-pointer has-disabled:cursor-default"
                >
                  <input
                    type="radio"
                    name="wf-lyrics-highlight"
                    value={choice}
                    checked={checked}
                    aria-label={label}
                    // Inert until the active profile's row has been read:
                    // the hook answers with the default meanwhile, so a
                    // click before that lands would persist a choice the
                    // user never made over one they did.
                    disabled={!ready}
                    onChange={() => {
                      void setId(choice === "default" ? null : choice);
                    }}
                    className="peer sr-only"
                  />
                  <span
                    aria-hidden="true"
                    className={`flex h-8 w-8 items-center justify-center rounded-full border border-zinc-300 dark:border-zinc-600 transition-shadow peer-focus-visible:ring-2 peer-focus-visible:ring-emerald-500 peer-focus-visible:ring-offset-2 dark:peer-focus-visible:ring-offset-zinc-900 ${
                      checked
                        ? "ring-2 ring-zinc-900 dark:ring-white ring-offset-2 ring-offset-white dark:ring-offset-zinc-900"
                        : ""
                    }`}
                    style={{ background: swatchBackground(choice) }}
                  >
                    {/* The mark, not just the ring, says which one is
                        chosen — a ring alone is a colour-only cue. Dark
                        on every swatch, since every swatch is light. */}
                    {checked && (
                      <Check
                        size={16}
                        strokeWidth={3}
                        className="text-zinc-900"
                      />
                    )}
                  </span>
                </label>
              );
            })}
          </div>
        </div>
      </div>
    </section>
  );
}

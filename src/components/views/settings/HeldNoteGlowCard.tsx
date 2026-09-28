import { useTranslation } from "react-i18next";
import { Sparkles } from "lucide-react";
import { useHeldNoteGlowSetting } from "../../../hooks/useHeldNoteGlow";
import { ToggleSwitch } from "../../common/ToggleSwitch";

/**
 * Settings → Lyrics row for the held-note ripple in the immersive lyrics.
 * Default on; this is where someone who finds it distracting turns it off
 * without asking the whole system for less motion.
 */
export function HeldNoteGlowCard() {
  const { t } = useTranslation();
  const { value, ready, setValue } = useHeldNoteGlowSetting();

  return (
    <section
      aria-label={t("settings.lyricsHeldNotes.title")}
      className="px-4 py-3"
    >
      <div className="flex items-center justify-between gap-3">
        <span className="flex items-start gap-3 min-w-0">
          <Sparkles
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.lyricsHeldNotes.title")}
            </span>
            <span className="block text-xs mt-0.5 settings-description">
              {t("settings.lyricsHeldNotes.subtitle")}
            </span>
          </span>
        </span>
        <ToggleSwitch
          enabled={value}
          onToggle={() => {
            void setValue(!value);
          }}
          label={t("settings.lyricsHeldNotes.title")}
          disabled={!ready}
        />
      </div>
    </section>
  );
}

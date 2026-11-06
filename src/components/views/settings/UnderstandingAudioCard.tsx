import { useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Activity,
  AudioWaveform,
  FileAudio,
  Headphones,
  Layers,
  Volume2,
  Waves,
  type LucideIcon,
} from "lucide-react";
import {
  PLAYER_BAR_LAYOUT_EVENT,
  PLAYER_BAR_LAYOUT_KEYS,
  usePlayerBarLayout,
} from "../../../hooks/usePlayerBarLayout";
import { setProfileSetting } from "../../../lib/tauri/profile";

/** The notions the card explains, in the order a file meets them. */
const NOTIONS: ReadonlyArray<{ key: string; icon: LucideIcon }> = [
  { key: "sampleRate", icon: Waves },
  { key: "bitDepth", icon: Layers },
  { key: "lossless", icon: FileAudio },
  { key: "resampling", icon: AudioWaveform },
  { key: "replayGain", icon: Volume2 },
  { key: "exclusive", icon: Headphones },
];

/**
 * Settings → Audio, first group: the handful of words the rest of the
 * page and the audio-quality strip use, each in two plain sentences, for
 * a listener who is not an audiophile. It ends on the strip itself —
 * where those words describe the track that is playing — with a button
 * that pins the strip under the player bar when it is hidden (the same
 * profile setting the player-bar layout card writes).
 *
 * The card keeps no state of its own: it is always open, so there is
 * nothing to remember and nothing for the "Reset" count to find.
 */
export function UnderstandingAudioCard() {
  const { t } = useTranslation();
  const layout = usePlayerBarLayout();
  const [busy, setBusy] = useState(false);

  const showStrip = async () => {
    setBusy(true);
    try {
      await setProfileSetting(
        PLAYER_BAR_LAYOUT_KEYS.showAudioQualityFooter,
        "true",
        "bool",
      );
      window.dispatchEvent(new CustomEvent(PLAYER_BAR_LAYOUT_EVENT));
    } catch (err) {
      console.error("[UnderstandingAudioCard] show strip failed", err);
    } finally {
      setBusy(false);
    }
  };

  // Two blocks of the group's card, so the card draws the hairline
  // between the notions and the pointer to the strip.
  return (
    <>
      <dl className="grid gap-x-6 gap-y-4 sm:grid-cols-2">
        {NOTIONS.map(({ key, icon: Icon }) => (
          <div key={key} className="flex items-start gap-3 min-w-0">
            <Icon size={16} className="settings-row-icon" aria-hidden="true" />
            <div className="min-w-0">
              <dt className="text-sm font-medium text-zinc-900 dark:text-white">
                {t(`settings.understandingAudio.${key}.term`)}
              </dt>
              <dd className="mt-0.5 text-xs settings-description">
                {t(`settings.understandingAudio.${key}.body`)}
              </dd>
            </div>
          </div>
        ))}
      </dl>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="flex min-w-0 items-start gap-3 text-xs settings-description">
          <Activity
            size={16}
            className="settings-row-icon"
            aria-hidden="true"
          />
          <span className="pt-2">
            {t(
              layout.showAudioQualityFooter
                ? "settings.understandingAudio.stripShown"
                : "settings.understandingAudio.stripHidden",
            )}
          </span>
        </p>
        {!layout.showAudioQualityFooter && (
          <button
            type="button"
            onClick={() => {
              void showStrip();
            }}
            disabled={busy}
            className="shrink-0 rounded-full bg-emerald-500 px-4 py-1.5 text-xs font-semibold text-white transition-colors hover:bg-emerald-600 disabled:opacity-50"
          >
            {t("settings.understandingAudio.showStrip")}
          </button>
        )}
      </div>
    </>
  );
}

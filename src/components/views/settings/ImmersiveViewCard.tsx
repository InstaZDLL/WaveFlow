import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { Columns2, Expand, type LucideIcon } from "lucide-react";
import {
  IMMERSIVE_PREF_KEYS,
  IMMERSIVE_PREFS_EVENT,
  useImmersivePrefs,
} from "../../../hooks/useImmersivePrefs";
import { setProfileSetting } from "../../../lib/tauri/profile";

/** The two writable toggles (excludes the derived `loaded` flag). */
type ImmersiveToggleKey = keyof typeof IMMERSIVE_PREF_KEYS;

/**
 * Settings → Appearance card for the immersive view (issue #328). Two
 * independent per-profile toggles:
 *  - merged lyrics column (vs the legacy single-view-with-toggle)
 *  - native OS fullscreen on open (vs an in-window overlay)
 *
 * Both default OFF so the immersive view stays the pre-#328 experience
 * out of the box; each is opt-in. Writes flow through
 * `setProfileSetting` then a single
 * `waveflow:immersive-prefs-changed` event so every mounted consumer
 * (the hook in `ImmersiveView`) re-reads in one go.
 */
export function ImmersiveViewCard() {
  const { t } = useTranslation();
  const prefs = useImmersivePrefs();
  const [busyKey, setBusyKey] = useState<string | null>(null);

  const writeBool = useCallback(
    async (prop: ImmersiveToggleKey, value: boolean) => {
      const key = IMMERSIVE_PREF_KEYS[prop];
      setBusyKey(key);
      try {
        await setProfileSetting(key, value ? "true" : "false", "bool");
        window.dispatchEvent(new CustomEvent(IMMERSIVE_PREFS_EVENT));
      } catch (err) {
        console.error(`[ImmersiveViewCard] set ${key} failed`, err);
      } finally {
        setBusyKey(null);
      }
    },
    [],
  );

  const rows: Array<{
    prop: ImmersiveToggleKey;
    icon: LucideIcon;
    title: string;
    subtitle: string;
  }> = [
    {
      prop: "mergedLyrics",
      icon: Columns2,
      title: t("settings.immersiveView.mergedLyrics.title"),
      subtitle: t("settings.immersiveView.mergedLyrics.subtitle"),
    },
    {
      prop: "useNativeFullscreen",
      icon: Expand,
      title: t("settings.immersiveView.nativeFullscreen.title"),
      subtitle: t("settings.immersiveView.nativeFullscreen.subtitle"),
    },
  ];

  return (
    <section
      aria-label={t("settings.immersiveView.title")}
      className="px-4 py-3"
    >
      <div className="text-sm font-medium text-zinc-900 dark:text-white mb-2">
        {t("settings.immersiveView.title")}
      </div>
      <div className="space-y-1">
        {rows.map(({ prop, icon: Icon, title, subtitle }) => {
          const key = IMMERSIVE_PREF_KEYS[prop];
          return (
            <label
              key={prop}
              className="flex items-start justify-between gap-3 cursor-pointer py-1"
            >
              <span className="flex items-start gap-3 min-w-0">
                <Icon
                  size={20}
                  className="text-zinc-400 mt-0.5 shrink-0"
                  aria-hidden="true"
                />
                <span className="min-w-0">
                  <span className="block text-sm text-zinc-900 dark:text-white">
                    {title}
                  </span>
                  <span className="block text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed mt-0.5">
                    {subtitle}
                  </span>
                </span>
              </span>
              <input
                type="checkbox"
                checked={prefs[prop]}
                disabled={busyKey === key}
                onChange={(e) => void writeBool(prop, e.target.checked)}
                className="mt-1.5 w-4 h-4 accent-emerald-500 cursor-pointer shrink-0 disabled:opacity-50"
                aria-label={title}
              />
            </label>
          );
        })}
      </div>
    </section>
  );
}

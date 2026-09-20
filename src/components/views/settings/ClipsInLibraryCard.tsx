import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { FolderHeart } from "lucide-react";

import {
  getClipsInLibrary,
  setClipsInLibrary,
} from "../../../lib/tauri/libraryMedia";
import { ToggleSwitch } from "../../common/ToggleSwitch";

/**
 * Settings → Storage and backups → Folders and caches: keep the Canvas
 * clips and animated covers you set **by hand** next to the music, in a
 * reserved folder inside the library, rather than in the app's data
 * directory (issue #695).
 *
 * Only the hand-set ones. What a plugin fetched stays in the LRU caches
 * above — an eviction pass deleting files out of somebody's music folder
 * is not a behaviour worth having.
 *
 * Nothing moves when the switch flips: clips are looked up in both places,
 * so the old ones keep playing where they are and the next ones land in
 * the new place. That is why there is no progress bar here and no
 * half-moved state to recover from.
 */
export function ClipsInLibraryCard() {
  const { t } = useTranslation();
  const [enabled, setEnabled] = useState(false);
  const [busy, setBusy] = useState(false);
  const [hydrated, setHydrated] = useState(false);
  // Guards the initial read against a click that beats it — the same
  // sub-10 ms window the other cards in this panel guard.
  const touched = useRef(false);

  useEffect(() => {
    let cancelled = false;
    getClipsInLibrary()
      .then((value) => {
        if (cancelled || touched.current) return;
        setEnabled(value);
      })
      .catch((err) => console.error("[ClipsInLibraryCard] read failed", err))
      .finally(() => {
        if (!cancelled) setHydrated(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const onToggle = async () => {
    if (busy) return;
    touched.current = true;
    const next = !enabled;
    setBusy(true);
    setEnabled(next);
    try {
      await setClipsInLibrary(next);
    } catch (err) {
      console.error("[ClipsInLibraryCard] write failed", err);
      setEnabled(!next);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="settings-row flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
      <div className="flex items-center space-x-4">
        <FolderHeart size={20} className="text-zinc-400" aria-hidden="true" />
        <div>
          <div className="text-sm font-medium text-zinc-900 dark:text-white">
            {t("settings.clipsInLibrary.title")}
          </div>
          <div className="text-xs settings-description">
            {t("settings.clipsInLibrary.subtitle")}
          </div>
        </div>
      </div>
      <ToggleSwitch
        enabled={enabled}
        onToggle={() => void onToggle()}
        disabled={!hydrated || busy}
        label={t("settings.clipsInLibrary.title")}
      />
    </div>
  );
}

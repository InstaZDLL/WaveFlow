import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Lock } from "lucide-react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import {
  playerGetWasapiExclusive,
  playerSetWasapiExclusive,
} from "../../../lib/tauri/player";
import { ToggleSwitch } from "../../common/ToggleSwitch";

/**
 * WASAPI Exclusive Mode card — Windows-only audiophile path.
 *
 * Detection: we check `navigator.userAgent` for "Windows" since the
 * setting is silently no-op on Linux / macOS and showing it there
 * would mislead users.
 *
 * The toggle calls the backend which:
 *   1. Persists the preference in `profile_setting`.
 *   2. Re-opens the audio output stream in exclusive event-driven mode.
 *   3. Falls back to cpal shared mode if exclusive init fails — the
 *      `getWasapiExclusive` read after the toggle reflects what's
 *      actually engaged so the UI never lies about the mode.
 */
export function ExclusiveModeCard() {
  const { t } = useTranslation();
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Windows-only gate. Sniffing UA is fine here — Tauri's WebView is
  // platform-pinned, so the result is stable for the lifetime of the
  // process.
  const isWindows =
    typeof navigator !== "undefined" &&
    navigator.userAgent.toLowerCase().includes("windows");

  useEffect(() => {
    if (!isWindows) return;
    playerGetWasapiExclusive()
      .then(setEnabled)
      .catch((err) => {
        console.error("[ExclusiveModeCard] get failed", err);
        setEnabled(false);
      });
  }, [isWindows]);

  // The engine can rebuild the output stream on its own — a device
  // flap (issue #405), a device switch from the output-device picker —
  // without the user ever touching this toggle. Without this listener
  // `enabled` only ever reflected the mount-time read or the last
  // manual click, so it could show "on" while a fallback had silently
  // dropped the engine to shared mode. `player:audio-mode-changed`
  // carries no payload; a re-fetch here mirrors the one `toggle()`
  // already does after a manual click.
  useEffect(() => {
    if (!isWindows) return;
    let unlisten: UnlistenFn | null = null;
    (async () => {
      try {
        unlisten = await listen("player:audio-mode-changed", () => {
          playerGetWasapiExclusive()
            .then(setEnabled)
            .catch((err) => {
              console.error("[ExclusiveModeCard] refresh after rebuild failed", err);
            });
        });
      } catch (err) {
        console.error("[ExclusiveModeCard] listen failed", err);
      }
    })();
    return () => {
      if (unlisten) unlisten();
    };
  }, [isWindows]);

  if (!isWindows) return null;

  const toggle = async (next: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await playerSetWasapiExclusive(next);
      // Re-read so the displayed state reflects the engine's actual
      // mode after fallback.
      const actual = await playerGetWasapiExclusive();
      setEnabled(actual);
      if (next && !actual) {
        setError(t("settings.exclusive.fallback"));
      }
    } catch (err) {
      console.error("[ExclusiveModeCard] toggle failed", err);
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
      <div className="flex items-center justify-between">
        <div className="flex items-center space-x-4">
          <Lock size={20} className="text-zinc-500 dark:text-zinc-400" />
          <div>
            <p className="text-sm font-medium text-zinc-800 dark:text-zinc-200">
              {t("settings.exclusive.title")}
            </p>
            <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">
              {t("settings.exclusive.subtitle")}
            </p>
          </div>
        </div>
        <ToggleSwitch
          enabled={enabled === true}
          onToggle={() => {
            if (busy || enabled === null) return;
            void toggle(!enabled);
          }}
          label={t("settings.exclusive.title")}
        />
      </div>
      {error && (
        <p className="text-xs text-amber-600 dark:text-amber-400 mt-2 ml-9">
          {error}
        </p>
      )}
    </div>
  );
}

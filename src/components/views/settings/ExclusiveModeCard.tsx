import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Lock } from "lucide-react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import {
  playerGetExclusiveOutput,
  playerSetExclusiveOutput,
} from "../../../lib/tauri/player";
import { ToggleSwitch } from "../../common/ToggleSwitch";

/**
 * Exclusive output card — the audiophile path where the app owns the
 * device instead of sharing it with the system mixer.
 *
 * Detection: we check `navigator.userAgent` for the platforms that
 * have a backend for it — Windows (WASAPI Exclusive) and Linux (a raw
 * ALSA `hw:` device). The setting is still a silent no-op on macOS,
 * whose exclusive backend carries DoP only, and showing a switch that
 * does nothing would mislead.
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

  // Sniffing UA is fine here — Tauri's WebView is platform-pinned, so
  // the result is stable for the lifetime of the process. macOS reports
  // "Macintosh", so it falls out of this test on its own.
  const supported =
    typeof navigator !== "undefined" &&
    ["windows", "linux"].some((os) =>
      navigator.userAgent.toLowerCase().includes(os),
    );

  useEffect(() => {
    if (!supported) return;
    playerGetExclusiveOutput()
      .then(setEnabled)
      .catch((err) => {
        console.error("[ExclusiveModeCard] get failed", err);
        setEnabled(false);
      });
  }, [supported]);

  // The engine can rebuild the output stream on its own — a device
  // flap (issue #405), a device switch from the output-device picker —
  // without the user ever touching this toggle. Without this listener
  // `enabled` only ever reflected the mount-time read or the last
  // manual click, so it could show "on" while a fallback had silently
  // dropped the engine to shared mode. `player:audio-mode-changed`
  // carries no payload; a re-fetch here mirrors the one `toggle()`
  // already does after a manual click.
  useEffect(() => {
    if (!supported) return;
    let unlisten: UnlistenFn | null = null;
    // `listen()` is async, so the effect can unmount before it resolves.
    // Without this flag the cleanup below runs while `unlisten` is still
    // null, does nothing, and the handle assigned afterward is never
    // released — a live listener leaks for the rest of the app's life
    // every time this card mounts and unmounts (opening/closing Settings).
    let cancelled = false;
    (async () => {
      try {
        const stop = await listen("player:audio-mode-changed", () => {
          playerGetExclusiveOutput()
            .then(setEnabled)
            .catch((err) => {
              console.error(
                "[ExclusiveModeCard] refresh after rebuild failed",
                err,
              );
            });
        });
        if (cancelled) {
          stop();
        } else {
          unlisten = stop;
        }
      } catch (err) {
        console.error("[ExclusiveModeCard] listen failed", err);
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, [supported]);

  if (!supported) return null;

  const toggle = async (next: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await playerSetExclusiveOutput(next);
      // Re-read so the displayed state reflects the engine's actual
      // mode after fallback.
      const actual = await playerGetExclusiveOutput();
      setEnabled(actual);
      if (next && !actual) {
        setError(t("settings.exclusive.fallback"));
      }
    } catch (err) {
      console.error("[ExclusiveModeCard] toggle failed", err);
      setError(String(err));
      // A failed toggle still moves the engine (issue #405): it may have
      // torn the old stream down before failing to open the new one. The
      // success path re-reads for exactly this reason — do it here too,
      // otherwise the switch keeps showing the mode the user just tried
      // to leave and looks stuck.
      try {
        setEnabled(await playerGetExclusiveOutput());
      } catch (refreshErr) {
        console.error(
          "[ExclusiveModeCard] refresh after failed toggle",
          refreshErr,
        );
      }
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

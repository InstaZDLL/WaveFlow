import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Lock } from "lucide-react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import {
  playerGetExclusiveOutputState,
  playerSetExclusiveOutput,
  type ExclusiveOutputState,
} from "../../../lib/tauri/player";
import { ToggleSwitch } from "../../common/ToggleSwitch";

/**
 * Exclusive output card — the audiophile path where the app owns the
 * device instead of sharing it with the system mixer.
 *
 * Shown on every desktop platform, because every one of them now has a
 * backend: WASAPI Exclusive on Windows, a raw ALSA `hw:` device on
 * Linux, hog mode on macOS. The card used to sniff the user agent to
 * hide itself where the toggle did nothing.
 *
 * The toggle calls the backend which:
 *   1. Persists the preference in `profile_setting`.
 *   2. Re-opens the audio output stream in exclusive event-driven mode.
 *   3. Falls back to cpal shared mode if exclusive init fails.
 *
 * The switch shows the **preference**, and a note says when it did not
 * engage. It used to show only what engaged, so after a refusal it read
 * "off" while exclusive stayed requested — every rebuild tried the device
 * again, and switching it "on" changed nothing, the preference being on
 * already. While it is engaged, a second note says the system volume no
 * longer applies: the device plays at its own hardware level, which is
 * wherever the sound server last left it and can be close to silent.
 */
export function ExclusiveModeCard() {
  const { t } = useTranslation();
  const [state, setState] = useState<ExclusiveOutputState | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    playerGetExclusiveOutputState()
      .then(setState)
      .catch((err) => {
        console.error("[ExclusiveModeCard] get failed", err);
        setState({ requested: false, engaged: false });
      });
  }, []);

  // The engine can rebuild the output stream on its own — a device
  // flap (issue #405), a device switch from the output-device picker —
  // without the user ever touching this toggle. Without this listener
  // the card only ever reflected the mount-time read or the last manual
  // click, and missed a fallback that silently dropped the engine to
  // shared mode. `player:audio-mode-changed`
  // carries no payload; a re-fetch here mirrors the one `toggle()`
  // already does after a manual click.
  useEffect(() => {
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
          playerGetExclusiveOutputState()
            .then(setState)
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
  }, []);

  const toggle = async (next: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await playerSetExclusiveOutput(next);
      // Re-read: whether it engaged is only known once the output has
      // been rebuilt, and the note below says so when it did not.
      setState(await playerGetExclusiveOutputState());
    } catch (err) {
      console.error("[ExclusiveModeCard] toggle failed", err);
      setError(String(err));
      // A failed toggle still moves the engine (issue #405): it may have
      // torn the old stream down before failing to open the new one. The
      // success path re-reads for exactly this reason — do it here too,
      // otherwise the switch keeps showing the mode the user just tried
      // to leave and looks stuck.
      try {
        setState(await playerGetExclusiveOutputState());
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
        <div className="flex items-center space-x-4 min-w-0">
          <Lock
            size={20}
            className="shrink-0 text-zinc-500 dark:text-zinc-400"
            aria-hidden="true"
          />
          <div className="min-w-0">
            <p className="text-sm font-medium text-zinc-800 dark:text-zinc-200">
              {t("settings.exclusive.title")}
            </p>
            <p className="text-xs mt-0.5 settings-description">
              {t("settings.exclusive.subtitle")}
            </p>
          </div>
        </div>
        <ToggleSwitch
          enabled={state?.requested === true}
          onToggle={() => {
            if (busy || state === null) return;
            void toggle(!state.requested);
          }}
          label={t("settings.exclusive.title")}
        />
      </div>
      {error ? (
        <p className="text-xs text-amber-600 dark:text-amber-400 mt-2 ml-9">
          {error}
        </p>
      ) : state?.requested && !state.engaged ? (
        <p className="text-xs text-amber-600 dark:text-amber-400 mt-2 ml-9">
          {t("settings.exclusive.fallback")}
        </p>
      ) : state?.engaged ? (
        <p className="text-xs settings-description mt-2 ml-9">
          {t("settings.exclusive.hardwareVolume")}
        </p>
      ) : null}
    </div>
  );
}

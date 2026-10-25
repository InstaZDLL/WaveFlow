import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Headphones } from "lucide-react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import {
  playerGetExclusiveOutputState,
  playerListOutputDevices,
  playerPlayTestTone,
  TEST_TONE_EVENT,
  type OutputSampleFormat,
  type TestToneEvent,
  type TestTonePhase,
} from "../../../lib/tauri/player";

/** The format the tone was generated at, and where it went. */
interface ToneOutput {
  sampleRate: number;
  channels: number;
  sampleFormat: OutputSampleFormat;
  mono: boolean;
  device: string | null;
  exclusive: boolean;
}

type Status = "idle" | "playing" | "finished" | "interrupted" | "failed";

const PHASES: TestTonePhase[] = ["left", "right", "both"];

/** Longer than the tone (about 3 s) plus a slow output's latency: past
 *  it, the decoder never answered and the button must not stay stuck. */
const NO_ANSWER_MS = 10_000;

const FORMAT_KEYS: Record<OutputSampleFormat, string> = {
  unknown: "unknown",
  f32: "f32",
  s32: "s32",
  s24: "s24",
  "s24-in-32": "s24In32",
  s16: "s16",
  u16: "s16",
  dop: "dop",
};

function formatRate(hz: number): string {
  // 48000 reads "48 kHz", 44100 "44.1 kHz".
  return `${(hz / 1000).toFixed(1).replace(/\.0$/, "")} kHz`;
}

/**
 * Output test tone: a beep on the left, on the right, then on both, at
 * the format the output stream really runs at, which the card then
 * spells out. Answers "is the DAC wired the right way round, and what is
 * it being sent?" without hunting for a test file.
 *
 * The beep on screen follows the output, not the button: the backend
 * reports each beep as the output reaches it.
 */
export function TestToneCard() {
  const { t } = useTranslation();
  const [status, setStatus] = useState<Status>("idle");
  const [phase, setPhase] = useState<TestTonePhase | null>(null);
  const [output, setOutput] = useState<ToneOutput | null>(null);
  const [error, setError] = useState<string | null>(null);
  const watchdog = useRef<number | null>(null);
  // Bumped by every press, so a slow device lookup from an earlier one
  // cannot overwrite the output of a later one.
  const press = useRef(0);

  const clearWatchdog = useCallback(() => {
    if (watchdog.current !== null) {
      window.clearTimeout(watchdog.current);
      watchdog.current = null;
    }
  }, []);

  const armWatchdog = useCallback(() => {
    clearWatchdog();
    watchdog.current = window.setTimeout(() => {
      watchdog.current = null;
      setPhase(null);
      setStatus("interrupted");
    }, NO_ANSWER_MS);
  }, [clearWatchdog]);

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    // `listen()` is async: the card can unmount before it resolves.
    let cancelled = false;
    (async () => {
      try {
        const stop = await listen<TestToneEvent>(TEST_TONE_EVENT, (event) => {
          const payload = event.payload;
          switch (payload.kind) {
            case "started": {
              const ticket = press.current;
              // The output may have taken a while to come back (a DoP
              // stream reopened as PCM): the tone starts now, whatever
              // the watchdog decided in the meantime.
              setStatus("playing");
              setError(null);
              armWatchdog();
              setOutput({
                sampleRate: payload.sampleRate,
                channels: payload.channels,
                sampleFormat: payload.sampleFormat,
                mono: payload.mono,
                device: null,
                exclusive: false,
              });
              // Read now rather than before the press: the tone may
              // have had to reopen the output first.
              Promise.all([
                playerListOutputDevices(),
                playerGetExclusiveOutputState(),
              ])
                .then(([devices, exclusive]) => {
                  if (press.current !== ticket) return;
                  const active = devices.find((d) => d.is_active);
                  setOutput((prev) =>
                    prev
                      ? {
                          ...prev,
                          device: active?.name ?? null,
                          exclusive: exclusive.engaged,
                        }
                      : prev,
                  );
                })
                .catch((err) => {
                  console.error("[TestToneCard] output lookup failed", err);
                });
              break;
            }
            case "phase":
              setPhase(payload.phase);
              break;
            case "finished":
              clearWatchdog();
              setPhase(null);
              setStatus("finished");
              break;
            case "interrupted":
              clearWatchdog();
              setPhase(null);
              setStatus("interrupted");
              break;
            case "failed":
              clearWatchdog();
              setPhase(null);
              setError(payload.message);
              setStatus("failed");
              break;
          }
        });
        if (cancelled) {
          stop();
        } else {
          unlisten = stop;
        }
      } catch (err) {
        console.error("[TestToneCard] listen failed", err);
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
      clearWatchdog();
    };
  }, [armWatchdog, clearWatchdog]);

  const play = async () => {
    press.current += 1;
    setStatus("playing");
    setPhase(null);
    setOutput(null);
    setError(null);
    armWatchdog();
    try {
      await playerPlayTestTone();
    } catch (err) {
      console.error("[TestToneCard] play failed", err);
      clearWatchdog();
      setError(String(err));
      setStatus("failed");
    }
  };

  const playing = status === "playing";
  const channelsLabel = (count: number) =>
    count === 1
      ? t("settings.testTone.mono")
      : count === 2
        ? t("settings.testTone.stereo")
        : t("settings.testTone.channels", { count });
  const details = output
    ? [
        output.device,
        formatRate(output.sampleRate),
        t(`settings.testTone.format.${FORMAT_KEYS[output.sampleFormat]}`),
        channelsLabel(output.channels),
        t(
          output.exclusive
            ? "settings.testTone.exclusive"
            : "settings.testTone.shared",
        ),
      ].filter((part): part is string => !!part)
    : [];

  return (
    <div className="settings-block">
      <div className="flex items-center justify-between gap-4">
        <div className="flex items-center space-x-4 min-w-0">
          <Headphones
            size={16}
            className="settings-row-icon"
            aria-hidden="true"
          />
          <div className="min-w-0">
            <p className="text-sm font-medium text-zinc-800 dark:text-zinc-200">
              {t("settings.testTone.title")}
            </p>
            <p className="text-xs mt-0.5 settings-description">
              {t("settings.testTone.subtitle")}
            </p>
          </div>
        </div>
        <button
          type="button"
          onClick={() => void play()}
          disabled={playing}
          className="shrink-0 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {playing
            ? t("settings.testTone.playing")
            : t("settings.testTone.play")}
        </button>
      </div>

      {status !== "idle" && status !== "failed" && output ? (
        <div className="mt-3 ml-9 space-y-2">
          <div className="flex flex-wrap gap-2" aria-live="polite">
            {PHASES.map((p) => (
              <span
                key={p}
                className={`px-3 py-1 rounded-full text-xs font-medium border transition-colors ${
                  phase === p
                    ? "border-emerald-500 bg-emerald-500 text-white"
                    : "border-zinc-200 text-zinc-500 dark:border-zinc-700 dark:text-zinc-400"
                }`}
                aria-current={phase === p ? "true" : undefined}
              >
                {t(`settings.testTone.${p}`)}
              </span>
            ))}
          </div>
          <p className="text-xs text-zinc-600 dark:text-zinc-300 tabular-nums">
            {details.join(" · ")}
          </p>
          {output.mono ? (
            <p className="text-xs text-amber-600 dark:text-amber-400">
              {t("settings.testTone.monoNote")}
            </p>
          ) : null}
          {!output.exclusive ? (
            <p className="text-xs settings-description">
              {t("settings.testTone.sharedNote")}
            </p>
          ) : null}
          {status === "finished" && !output.mono && output.channels >= 2 ? (
            <p className="text-xs settings-description">
              {t("settings.testTone.swapped")}
            </p>
          ) : null}
        </div>
      ) : null}

      {status === "interrupted" ? (
        <p className="text-xs text-amber-600 dark:text-amber-400 mt-2 ml-9">
          {t("settings.testTone.interrupted")}
        </p>
      ) : null}

      {status === "failed" ? (
        <p className="text-xs text-amber-600 dark:text-amber-400 mt-2 ml-9">
          {t("settings.testTone.failed", { message: error ?? "" })}
        </p>
      ) : null}
    </div>
  );
}

import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AudioLines } from "lucide-react";

import {
  remoteGetStatus,
  remoteTranscodeStatus,
  type RemoteTranscodeStatus,
} from "../../../lib/tauri/remoteServer";
import {
  BITRATE_CHOICES,
  useRemoteTranscode,
  type RemoteTranscodeFormat,
} from "../../../hooks/useRemoteTranscode";

const FORMATS: ReadonlyArray<{
  id: RemoteTranscodeFormat;
  labelKey: string;
  hintKey: string;
}> = [
  {
    id: "off",
    labelKey: "settings.remoteTranscode.off",
    hintKey: "settings.remoteTranscode.offHint",
  },
  {
    id: "opus",
    labelKey: "settings.remoteTranscode.opus",
    hintKey: "settings.remoteTranscode.opusHint",
  },
  {
    id: "mp3",
    labelKey: "settings.remoteTranscode.mp3",
    hintKey: "settings.remoteTranscode.mp3Hint",
  },
];

/**
 * Settings → ask the server to re-encode the remote source before sending it.
 *
 * The stream route has always accepted `format` and `bitrate`; the desktop
 * simply never sent them, so every remote track arrived as the original file.
 * This is the control that starts sending them.
 *
 * **Remote source only, and off by default.** A local file is already on the
 * machine — re-encoding it would cost quality and buy nothing — and
 * transcoding trades fidelity for bandwidth, which is a trade nobody should
 * be opted into silently. When it is on, the card says so plainly rather than
 * leaving the degradation to be inferred.
 *
 * Hides itself when `sync_v2` is absent or no server is bound, like every
 * other remote surface: TypeScript cannot see a Cargo feature, so the card
 * probes for its backend.
 */
export function RemoteTranscodeCard() {
  const { t } = useTranslation();
  const { format, bitrate, ready, setFormat, setBitrate } =
    useRemoteTranscode();
  const [visible, setVisible] = useState(false);
  const [server, setServer] = useState<RemoteTranscodeStatus | null>(null);
  const mountedRef = useRef(false);

  useEffect(() => {
    mountedRef.current = true;
    void (async () => {
      try {
        const status = await remoteGetStatus();
        if (!mountedRef.current) return;
        const nextVisible = status.signed_in;
        setVisible(nextVisible);
        if (!nextVisible) return;
      } catch {
        if (mountedRef.current) setVisible(false);
        return;
      }
      // Its own catch, deliberately. This one is a network call to a server
      // that may be asleep, off the LAN, or simply behind an offline switch —
      // none of which is a reason to take the preference away. Sharing the
      // block above would have hidden the whole card exactly when someone is
      // most likely to be setting it: before leaving the network.
      try {
        const transcode = await remoteTranscodeStatus();
        if (mountedRef.current) setServer(transcode);
      } catch {
        // Leave `server` null: the ceilings are informational, and the
        // backend falls back to the original bytes on its own.
      }
    })();
    return () => {
      mountedRef.current = false;
    };
  }, []);

  if (!visible || !ready) return null;

  const isOn = format !== "off";
  // A server built without FFmpeg cannot honour the preference however it is
  // set. Saying so is the difference between a setting that does nothing and
  // a setting that does nothing *for a stated reason*.
  const unsupported = server != null && !server.available;

  return (
    <section
      aria-labelledby="settings-remote-transcode-heading"
      className="px-4 py-3"
    >
      <header className="flex items-start gap-3 mb-3">
        <AudioLines
          size={20}
          className="text-zinc-400 mt-0.5 shrink-0"
          aria-hidden="true"
        />
        <div className="min-w-0">
          <h3
            id="settings-remote-transcode-heading"
            className="text-sm font-medium text-zinc-900 dark:text-white"
          >
            {t("settings.remoteTranscode.title")}
          </h3>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed mt-0.5">
            {t("settings.remoteTranscode.subtitle")}
          </p>
        </div>
      </header>

      <div
        role="radiogroup"
        aria-labelledby="settings-remote-transcode-heading"
        className="grid grid-cols-1 sm:grid-cols-3 gap-2"
      >
        {FORMATS.map(({ id, labelKey, hintKey }) => {
          const selected = format === id;
          return (
            <button
              key={id}
              type="button"
              role="radio"
              aria-checked={selected}
              disabled={unsupported && id !== "off"}
              onClick={() => setFormat(id)}
              className={[
                "flex flex-col items-start gap-1 rounded-xl border p-3 text-left transition-all disabled:opacity-50 disabled:cursor-not-allowed",
                selected
                  ? "border-emerald-500 bg-emerald-50 dark:bg-emerald-950/30 ring-1 ring-emerald-500/40"
                  : "border-zinc-200 dark:border-zinc-700 hover:border-zinc-300 dark:hover:border-zinc-600 bg-white dark:bg-zinc-900",
              ].join(" ")}
            >
              <span className="text-sm font-medium text-zinc-900 dark:text-white">
                {t(labelKey)}
              </span>
              <span className="text-xs text-zinc-500 dark:text-zinc-400 leading-snug">
                {t(hintKey)}
              </span>
            </button>
          );
        })}
      </div>

      {isOn && (
        <label className="mt-3 flex items-center justify-between gap-3">
          <span className="text-sm text-zinc-700 dark:text-zinc-300">
            {t("settings.remoteTranscode.bitrateLabel")}
          </span>
          <select
            value={bitrate}
            onChange={(e) => setBitrate(Number(e.target.value))}
            className="rounded-lg border border-zinc-200 bg-white px-3 py-1.5 text-sm text-zinc-800 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
          >
            {/* A stored rate the codec does not offer still has to appear, or
                the select would show a value the list cannot account for. */}
            {[...new Set([...BITRATE_CHOICES[format], bitrate])]
              .sort((a, b) => a - b)
              .map((rate) => (
                <option key={rate} value={rate}>
                  {t("settings.remoteTranscode.kbps", { rate })}
                </option>
              ))}
          </select>
        </label>
      )}

      {/* The degradation is stated, not left to be noticed. */}
      {isOn && !unsupported && (
        <p className="mt-3 text-xs text-amber-600 dark:text-amber-400 leading-relaxed">
          {t("settings.remoteTranscode.activeNotice")}
        </p>
      )}

      {unsupported && (
        <p
          role="status"
          className="mt-3 text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed"
        >
          {t("settings.remoteTranscode.unsupported")}
        </p>
      )}
    </section>
  );
}

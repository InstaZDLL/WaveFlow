import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AnimatePresence, motion } from "framer-motion";
import { Volume2, Speaker, Check, Loader2, Info } from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";
import {
  playerProbeOutputDevice,
  playerReopenOutputDevice,
  playerSetOutputDevice,
  type DeviceCapabilities,
  type OutputDevice,
} from "../../lib/tauri/player";
import {
  bestFormatPair,
  rateKHz,
  rateTiers,
} from "../../lib/deviceCapabilities";
import { isHiRes } from "../../lib/hiRes";

/** Translator, narrowed to what this file uses. */
type Translator = (key: string, options?: Record<string, unknown>) => string;

/** Which question the answer came from, in words (#593). */
const SOURCE_KEY: Record<DeviceCapabilities["source"], string> = {
  "wasapi-exclusive": "deviceMenu.capabilities.source.wasapiExclusive",
  "alsa-hardware": "deviceMenu.capabilities.source.alsaHardware",
  coreaudio: "deviceMenu.capabilities.source.coreAudio",
  unavailable: "deviceMenu.capabilities.source.unavailable",
};

/**
 * The one line a row carries: the deepest format, the top rate **that
 * format runs at**, and whether that pair is Hi-Res. Enough to choose
 * between three outputs without opening anything — and a pair the device
 * really accepts, rather than two maxima it may never have combined.
 */
function CapabilitySummary({
  caps,
  t,
}: {
  caps: DeviceCapabilities | undefined;
  t: Translator;
}) {
  if (caps == null) {
    return (
      <span className="text-[10px] opacity-60">
        {t("deviceMenu.capabilities.probing")}
      </span>
    );
  }
  const best = bestFormatPair(caps);
  if (caps.source === "unavailable" || best == null) {
    return (
      <span
        className="text-[10px] opacity-60"
        // The backend's own words, on demand and never in the layout —
        // same rule as the playback alerts (#597).
        title={caps.unavailable_reason ?? undefined}
      >
        {t("deviceMenu.capabilities.unavailable")}
      </span>
    );
  }
  return (
    <span className="text-[10px] opacity-70 flex items-center gap-1.5">
      <span className="tabular-nums">
        {t("deviceMenu.capabilities.summary", {
          bits: best.bits,
          rate: rateKHz(best.rate),
        })}
      </span>
      {isHiRes(best.bits, best.rate) && (
        <span className="px-1 rounded bg-emerald-500/15 text-emerald-600 dark:text-emerald-400 font-semibold">
          Hi-Res
        </span>
      )}
    </span>
  );
}

/**
 * The sheet behind the row: identity, the formats as a set, the rates as
 * named tiers, and the caveat that makes the whole thing honest.
 */
function CapabilitySheet({
  caps,
  t,
}: {
  caps: DeviceCapabilities | undefined;
  t: Translator;
}) {
  if (caps == null) {
    return (
      <div className="px-4 pb-3 text-xs flex items-center gap-2 opacity-70">
        <Loader2 size={12} className="animate-spin" />
        <span>{t("deviceMenu.capabilities.probing")}</span>
      </div>
    );
  }

  const tiers = rateTiers(caps.sample_rates);
  return (
    <div className="px-4 pb-3 pt-1 text-xs space-y-2 text-zinc-600 dark:text-zinc-300">
      {caps.source === "unavailable" ? (
        <p title={caps.unavailable_reason ?? undefined}>
          {t("deviceMenu.capabilities.unavailable")}
        </p>
      ) : (
        <>
          <div className="flex flex-wrap gap-x-3 gap-y-1 tabular-nums">
            <span>
              {t("deviceMenu.capabilities.channels", {
                value: caps.max_channels,
              })}
            </span>
            {caps.buffer_frames != null && (
              <span>
                {t("deviceMenu.capabilities.buffer", {
                  value: caps.buffer_frames,
                })}
              </span>
            )}
          </div>

          {caps.formats.length > 0 && (
            <div>
              <div className="uppercase tracking-wide text-[10px] opacity-60">
                {t("deviceMenu.capabilities.formats")}
              </div>
              <div className="flex flex-wrap gap-1 mt-1">
                {caps.formats.map((format) => (
                  <span
                    key={format.label}
                    className="px-1.5 py-0.5 rounded bg-zinc-500/10 font-mono text-[10px]"
                  >
                    {format.label}
                  </span>
                ))}
              </div>
            </div>
          )}

          {tiers.length > 0 && (
            <div>
              <div className="uppercase tracking-wide text-[10px] opacity-60">
                {t("deviceMenu.capabilities.rates")}
              </div>
              <div className="flex flex-wrap gap-1 mt-1">
                {tiers.map((tier) => (
                  <span
                    key={tier.key}
                    className="px-1.5 py-0.5 rounded bg-zinc-500/10 text-[10px]"
                    // The numbers are still there for whoever wants
                    // them; the tier is what the list reads as.
                    title={tier.rates
                      .map((rate) => `${rateKHz(rate)} kHz`)
                      .join(" · ")}
                  >
                    {t(`deviceMenu.capabilities.tier.${tier.key}`)}
                  </span>
                ))}
              </div>
            </div>
          )}
        </>
      )}

      <p className="opacity-60 leading-snug">
        {t(SOURCE_KEY[caps.source])}{" "}
        {caps.source !== "unavailable" && t("deviceMenu.capabilities.caveat")}
      </p>
    </div>
  );
}

export function DeviceMenu() {
  const { t } = useTranslation();
  const { isDeviceMenuOpen, outputDevices, refreshOutputDevices } = usePlayer();
  const [switching, setSwitching] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [capabilities, setCapabilities] = useState<
    Record<string, DeviceCapabilities>
  >({});
  const [expanded, setExpanded] = useState<string | null>(null);
  // The same map, readable without making the probe effect depend on it:
  // depending on the state it fills would restart the walk on every
  // answer.
  const capabilitiesRef = useRef<Record<string, DeviceCapabilities>>({});

  // The list itself is pre-fetched at boot in `PlayerContext` so
  // first paint is instant. We still re-poll in the background each
  // time the menu opens to catch hot-plugged USB DACs / Bluetooth
  // sinks attached since the last open — the call costs ~10 ms on
  // Linux thanks to the ALSA-hint enumeration, no extra waiting.
  useEffect(() => {
    if (!isDeviceMenuOpen) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setError(null);
    void refreshOutputDevices();
  }, [isDeviceMenuOpen, refreshOutputDevices]);

  // Capabilities are asked for one device at a time, and only once the
  // menu is open (#593): the listing itself must never probe, which is
  // the whole reason it reads ALSA's hints rather than opening every PCM.
  // Sequential rather than parallel — each answer costs a COM round trip
  // per format on Windows, or an open on Linux — and the rows fill in as
  // they arrive. The backend memoises, so a second open is free.
  useEffect(() => {
    if (!isDeviceMenuOpen) return;
    let cancelled = false;
    void (async () => {
      for (const device of outputDevices) {
        if (cancelled) return;
        if (capabilitiesRef.current[device.id] != null) continue;
        try {
          const caps = await playerProbeOutputDevice(device.id);
          if (cancelled) return;
          capabilitiesRef.current = {
            ...capabilitiesRef.current,
            [device.id]: caps,
          };
          setCapabilities(capabilitiesRef.current);
        } catch (err) {
          console.error("[DeviceMenu] probe failed", device.id, err);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [isDeviceMenuOpen, outputDevices]);

  const activeDevice = outputDevices.find((d) => d.is_active) ?? null;

  const handleSelect = async (device: OutputDevice) => {
    if (switching != null) return;
    setSwitching(device.id);
    try {
      // Clicking the row that is already playing means "open it again".
      // The pick itself would be a no-op in the engine, which is exactly
      // why a user whose audio had drifted elsewhere had to select
      // another device and come back (#612).
      if (device.is_active) {
        await playerReopenOutputDevice();
      } else {
        await playerSetOutputDevice(device.id);
      }
      // Refresh so the active flag follows what actually opened. The
      // backend already persisted the pick + restarted the cpal
      // stream by the time this resolves.
      await refreshOutputDevices();
    } catch (err) {
      console.error("[DeviceMenu] set device failed", err);
      setError(String(err));
    } finally {
      setSwitching(null);
    }
  };

  const isEmpty = outputDevices.length === 0;

  return (
    <AnimatePresence>
      {isDeviceMenuOpen && (
        <motion.div
          initial={{ opacity: 0, scale: 0.96, y: 8 }}
          animate={{ opacity: 1, scale: 1, y: 0 }}
          exit={{ opacity: 0, scale: 0.97, y: 4 }}
          transition={{
            type: "spring",
            stiffness: 480,
            damping: 30,
            mass: 0.5,
          }}
          style={{ transformOrigin: "bottom right" }}
          className="absolute bottom-4 right-20 w-96 rounded-xl shadow-2xl z-50 border py-2 flex flex-col max-h-[60vh] overflow-y-auto bg-white border-zinc-200 text-zinc-800 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-200"
        >
          <div className="px-4 py-2 text-sm font-semibold flex items-center space-x-2 text-emerald-500 bg-emerald-500/10 mb-1">
            <Volume2 size={16} aria-hidden="true" />
            <span className="truncate">
              {activeDevice?.name ?? t("deviceMenu.activeLabel")}
            </span>
          </div>

          {error && (
            <div className="px-4 py-3 text-sm text-red-500">{error}</div>
          )}

          {isEmpty && !error && (
            <div className="px-4 py-3 flex items-center space-x-2 text-sm text-zinc-500 dark:text-zinc-400">
              <Loader2 size={14} className="animate-spin" />
              <span>{t("deviceMenu.loading")}</span>
            </div>
          )}

          {outputDevices.map((device) => {
            const isSwitching = switching === device.id;
            const isExpanded = expanded === device.id;
            return (
              <div key={device.id} className="flex flex-col">
                <div
                  className={`flex items-center transition-colors ${
                    device.is_active
                      ? "text-emerald-500 hover:bg-emerald-500/10"
                      : "hover:bg-emerald-500 hover:text-white"
                  }`}
                >
                  <button
                    type="button"
                    onClick={() => handleSelect(device)}
                    disabled={switching != null}
                    title={device.is_active ? t("deviceMenu.reopen") : undefined}
                    className="px-4 py-2 text-sm cursor-pointer flex items-center space-x-3 text-left disabled:opacity-60 disabled:cursor-wait flex-1 min-w-0"
                  >
                    <Speaker size={16} className="opacity-70 shrink-0" />
                    <span className="flex flex-col min-w-0 flex-1">
                      <span className="truncate">{device.name}</span>
                      <CapabilitySummary caps={capabilities[device.id]} t={t} />
                    </span>
                    {device.is_pinned && !device.is_active && (
                      <span className="text-[10px] uppercase tracking-wide text-amber-500 shrink-0">
                        {t("deviceMenu.pinnedUnavailable")}
                      </span>
                    )}
                    {device.is_default &&
                      !device.is_active &&
                      !device.is_pinned && (
                        <span className="text-[10px] uppercase tracking-wide text-zinc-400 dark:text-zinc-500 shrink-0">
                          {t("deviceMenu.systemDefault")}
                        </span>
                      )}
                    {isSwitching ? (
                      <Loader2 size={14} className="animate-spin shrink-0" />
                    ) : device.is_active ? (
                      <Check size={16} className="shrink-0" />
                    ) : null}
                  </button>
                  <button
                    type="button"
                    onClick={() => setExpanded(isExpanded ? null : device.id)}
                    aria-expanded={isExpanded}
                    aria-label={t("deviceMenu.capabilities.details")}
                    title={t("deviceMenu.capabilities.details")}
                    className={`px-3 py-2 shrink-0 hover:opacity-100 ${
                      isExpanded ? "opacity-100" : "opacity-60"
                    }`}
                  >
                    <Info size={14} />
                  </button>
                </div>
                {isExpanded && (
                  <CapabilitySheet caps={capabilities[device.id]} t={t} />
                )}
              </div>
            );
          })}
        </motion.div>
      )}
    </AnimatePresence>
  );
}

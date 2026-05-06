import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Volume2, Speaker, Check, Loader2 } from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";
import {
  playerListOutputDevices,
  playerSetOutputDevice,
  type OutputDevice,
} from "../../lib/tauri/player";

export function DeviceMenu() {
  const { t } = useTranslation();
  const { isDeviceMenuOpen } = usePlayer();
  const [devices, setDevices] = useState<OutputDevice[]>([]);
  const [loading, setLoading] = useState(false);
  const [switching, setSwitching] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Re-enumerate every time the menu opens so newly attached USB
  // DACs / Bluetooth sinks show up without restarting the app. cpal
  // doesn't push device-change events on its own, so polling on
  // demand is the lightest workable option.
  useEffect(() => {
    if (!isDeviceMenuOpen) return;
    let cancelled = false;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLoading(true);
    setError(null);
    playerListOutputDevices()
      .then((list) => {
        if (!cancelled) setDevices(list);
      })
      .catch((err) => {
        if (!cancelled) {
          console.error("[DeviceMenu] list devices failed", err);
          setError(String(err));
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [isDeviceMenuOpen]);

  if (!isDeviceMenuOpen) return null;

  const activeDevice = devices.find((d) => d.is_active) ?? null;

  const handleSelect = async (device: OutputDevice) => {
    if (device.is_active || switching != null) return;
    setSwitching(device.id);
    try {
      await playerSetOutputDevice(device.id);
      // Refresh the list so the active flag moves to the new row.
      const next = await playerListOutputDevices();
      setDevices(next);
    } catch (err) {
      console.error("[DeviceMenu] set device failed", err);
      setError(String(err));
    } finally {
      setSwitching(null);
    }
  };

  return (
    <div className="absolute bottom-4 right-20 w-96 rounded-xl shadow-2xl z-50 border py-2 flex flex-col max-h-[60vh] overflow-y-auto bg-white border-zinc-200 text-zinc-800 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-200">
      <div className="px-4 py-2 text-sm font-semibold flex items-center space-x-2 text-emerald-500 bg-emerald-500/10 mb-1">
        <Volume2 size={16} aria-hidden="true" />
        <span className="truncate">
          {activeDevice?.name ?? t("deviceMenu.activeLabel")}
        </span>
      </div>

      {loading && (
        <div className="px-4 py-3 flex items-center space-x-2 text-sm text-zinc-500 dark:text-zinc-400">
          <Loader2 size={14} className="animate-spin" />
          <span>{t("deviceMenu.loading")}</span>
        </div>
      )}

      {!loading && error && (
        <div className="px-4 py-3 text-sm text-red-500">{error}</div>
      )}

      {!loading && !error && devices.length === 0 && (
        <div className="px-4 py-3 text-sm text-zinc-500 dark:text-zinc-400">
          {t("deviceMenu.empty")}
        </div>
      )}

      {!loading &&
        !error &&
        devices.map((device) => {
          const isSwitching = switching === device.id;
          return (
            <button
              key={device.id}
              type="button"
              onClick={() => handleSelect(device)}
              disabled={device.is_active || switching != null}
              className={`px-4 py-2 text-sm cursor-pointer flex items-center space-x-3 transition-colors text-left ${
                device.is_active
                  ? "text-emerald-500 cursor-default"
                  : "hover:bg-emerald-500 hover:text-white disabled:opacity-60 disabled:cursor-wait"
              }`}
            >
              <Speaker size={16} className="opacity-70 shrink-0" />
              <span className="truncate flex-1">{device.name}</span>
              {device.is_default && !device.is_active && (
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
          );
        })}
    </div>
  );
}

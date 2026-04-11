import { useTranslation } from "react-i18next";
import { Volume2, Speaker } from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";

export function DeviceMenu() {
  const { t } = useTranslation();
  const { isDeviceMenuOpen } = usePlayer();

  if (!isDeviceMenuOpen) return null;

  return (
    <div className="absolute bottom-4 right-20 w-80 rounded-xl shadow-2xl z-50 border py-2 flex flex-col max-h-[60vh] overflow-y-auto bg-white border-zinc-200 text-zinc-800 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-200">
      <div className="px-4 py-2 text-sm font-semibold flex items-center space-x-2 text-emerald-500 bg-emerald-500/10 mb-1">
        <Volume2 size={16} aria-hidden="true" />
        <span>{t("deviceMenu.activeLabel")}</span>
      </div>
      {Array.from({ length: 15 }, (_, i) => (
        <div
          key={i}
          className="px-4 py-2 text-sm hover:bg-emerald-500 hover:text-white cursor-pointer flex items-center space-x-3 transition-colors"
        >
          <Speaker size={16} className="opacity-70" />
          <span className="truncate">HDA Intel PCH, ALC897 Analog...</span>
        </div>
      ))}
    </div>
  );
}

import { useState, useRef, useEffect } from "react";
import { useTranslation } from "react-i18next";
import {
  ChevronLeft,
  ChevronRight,
  Search,
  Sun,
  Moon,
  Users,
  BarChart2,
  Settings,
  MessageSquare,
  Info,
  LogOut,
  ChevronDown,
  ChevronUp,
} from "lucide-react";
import type { ViewId } from "../../types";
import { useTheme } from "../../hooks/useTheme";
import { useProfile } from "../../hooks/useProfile";
import { getProfileColor, profileInitial } from "../../lib/profileColors";
import { MenuActionItem } from "../common/MenuActionItem";

interface TopBarProps {
  activeView: ViewId;
  setActiveView: (view: ViewId) => void;
  onOpenProfileSelector: () => void;
  canGoBack: boolean;
  canGoForward: boolean;
  onGoBack: () => void;
  onGoForward: () => void;
}

export function TopBar({
  setActiveView,
  onOpenProfileSelector,
  canGoBack,
  canGoForward,
  onGoBack,
  onGoForward,
}: TopBarProps) {
  const { t } = useTranslation();
  const { isDark, toggleTheme } = useTheme();
  const { activeProfile } = useProfile();
  const profileColor = getProfileColor(activeProfile?.color_id);
  const profileName = activeProfile?.name ?? "";
  const profileLetter = activeProfile ? profileInitial(activeProfile.name) : "";
  const [isProfileOpen, setIsProfileOpen] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);

  // Close dropdown on click outside
  useEffect(() => {
    if (!isProfileOpen) return;
    const handleClick = (e: MouseEvent) => {
      if (dropdownRef.current && !dropdownRef.current.contains(e.target as Node)) {
        setIsProfileOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [isProfileOpen]);

  const handleMenuNav = (view: ViewId) => {
    setActiveView(view);
    setIsProfileOpen(false);
  };

  const handleQuit = async () => {
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      getCurrentWindow().close();
    } catch {
      window.close();
    }
  };

  return (
    <div className="h-20 flex items-center justify-between px-8 z-10 sticky top-0 bg-zinc-50/80 backdrop-blur-md dark:bg-zinc-900/80">
      {/* Navigation Arrows */}
      <div className="flex space-x-2">
        <button
          onClick={onGoBack}
          disabled={!canGoBack}
          className={`p-2 rounded-full border transition-colors border-zinc-200 bg-white dark:border-zinc-700 dark:bg-zinc-800 ${
            canGoBack
              ? "text-zinc-600 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-white"
              : "text-zinc-300 cursor-not-allowed dark:text-zinc-600"
          }`}
        >
          <ChevronLeft size={20} />
        </button>
        <button
          onClick={onGoForward}
          disabled={!canGoForward}
          className={`p-2 rounded-full border transition-colors border-zinc-200 bg-white dark:border-zinc-700 dark:bg-zinc-800 ${
            canGoForward
              ? "text-zinc-600 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-white"
              : "text-zinc-300 cursor-not-allowed dark:text-zinc-600"
          }`}
        >
          <ChevronRight size={20} />
        </button>
      </div>

      {/* Search Bar */}
      <div className="flex-1 max-w-xl mx-8">
        <div className="flex items-center px-4 py-2.5 rounded-full border transition-all focus-within:ring-2 ring-emerald-500/20 bg-white border-zinc-200 dark:bg-zinc-800/50 dark:border-zinc-700 dark:text-zinc-200">
          <Search size={18} className="text-zinc-400 mr-3" />
          <input
            type="text"
            placeholder={t("topbar.search.placeholder")}
            className="bg-transparent border-none outline-none w-full text-sm placeholder-zinc-400"
          />
        </div>
      </div>

      {/* Right Actions */}
      <div className="flex items-center space-x-4">
        {/* Theme Toggle */}
        <button
          type="button"
          onClick={(e) => toggleTheme(e)}
          aria-label={isDark ? t("topbar.theme.enableLight") : t("topbar.theme.enableDark")}
          aria-pressed={isDark}
          className={`relative w-14 h-8 rounded-full border transition-colors duration-500 ease-in-out ${
            isDark
              ? "bg-zinc-800 border-zinc-700"
              : "bg-white border-zinc-300"
          }`}
        >
          <div
            className={`absolute top-1 left-1 w-6 h-6 rounded-full flex items-center justify-center transition-all duration-500 ease-in-out ${
              isDark
                ? "translate-x-6 bg-zinc-700 text-yellow-400"
                : "translate-x-0 bg-zinc-100 text-amber-500"
            }`}
          >
            <Sun
              size={14}
              className={`absolute transition-all duration-500 ${
                isDark
                  ? "opacity-0 rotate-90 scale-50"
                  : "opacity-100 rotate-0 scale-100"
              }`}
            />
            <Moon
              size={14}
              className={`absolute transition-all duration-500 ${
                isDark
                  ? "opacity-100 rotate-0 scale-100"
                  : "opacity-0 -rotate-90 scale-50"
              }`}
            />
          </div>
        </button>

        {/* Profile Dropdown */}
        <div className="relative" ref={dropdownRef}>
          <button
            onClick={() => setIsProfileOpen(!isProfileOpen)}
            className={`flex items-center space-x-2 px-3 py-1.5 rounded-full border transition-colors
              ${
                isProfileOpen
                  ? "border-zinc-300 bg-zinc-100 text-zinc-800 dark:border-zinc-600 dark:bg-zinc-700 dark:text-zinc-200"
                  : "border-zinc-200 bg-white hover:bg-zinc-50 text-zinc-700 dark:border-zinc-700 dark:bg-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-200"
              }`}
          >
            <div
              className={`w-6 h-6 rounded-full ${profileColor.avatarBg} ${profileColor.avatarText} flex items-center justify-center text-xs font-bold`}
            >
              {profileLetter}
            </div>
            <span className="text-sm font-medium">{profileName}</span>
            {isProfileOpen ? (
              <ChevronUp size={14} className="text-zinc-400" />
            ) : (
              <ChevronDown size={14} className="text-zinc-400" />
            )}
          </button>

          {isProfileOpen && (
            <div className="absolute top-full right-0 mt-2 w-56 rounded-xl shadow-lg border overflow-hidden z-50 bg-white border-zinc-200 dark:bg-zinc-800 dark:border-zinc-700 animate-fade-in">
              {/* Profile Header */}
              <div className="p-4 flex items-center space-x-3">
                <div
                  className={`w-10 h-10 rounded-full ${profileColor.avatarBg} ${profileColor.avatarText} flex items-center justify-center font-bold text-lg shadow-sm`}
                >
                  {profileLetter}
                </div>
                <div className="flex flex-col text-left min-w-0">
                  <div className="font-semibold text-sm text-zinc-900 dark:text-white truncate">
                    {profileName}
                  </div>
                  <div className="text-xs text-zinc-400">{t("topbar.profile.user")}</div>
                </div>
              </div>

              <div className="border-t py-2 border-zinc-100 dark:border-zinc-700">
                <MenuActionItem
                  icon={<Users size={16} />}
                  label={t("topbar.profile.changeProfile")}
                  onClick={() => {
                    onOpenProfileSelector();
                    setIsProfileOpen(false);
                  }}
                />
                <MenuActionItem
                  icon={<BarChart2 size={16} />}
                  label={t("topbar.profile.statistics")}
                  onClick={() => handleMenuNav("statistics")}
                />
                <MenuActionItem
                  icon={<Settings size={16} />}
                  label={t("topbar.profile.settings")}
                  onClick={() => handleMenuNav("settings")}
                />
                <MenuActionItem
                  icon={<MessageSquare size={16} />}
                  label={t("topbar.profile.feedback")}
                  onClick={() => handleMenuNav("feedback")}
                />
                <MenuActionItem
                  icon={<Info size={16} />}
                  label={t("topbar.profile.about")}
                  onClick={() => handleMenuNav("about")}
                />
              </div>

              <div className="border-t py-2 border-zinc-100 dark:border-zinc-700">
                <MenuActionItem
                  icon={<LogOut size={16} />}
                  label={t("topbar.profile.quit")}
                  danger
                  onClick={handleQuit}
                />
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

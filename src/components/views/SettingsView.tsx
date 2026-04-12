import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowLeft,
  Globe,
  Power,
  Minimize2,
  ScanLine,
  RefreshCcw,
  ImageIcon,
  FolderOpen,
  Trash2,
  ChevronDown,
  Check,
  Volume2,
  Headphones,
  Shuffle,
  Radio,
  Eye,
  EyeOff,
} from "lucide-react";
import type { ViewId } from "../../types";
import { SUPPORTED_LANGUAGES } from "../../i18n";
import {
  playerGetAudioSettings,
  playerSetNormalize,
  playerSetMono,
  playerSetCrossfade,
} from "../../lib/tauri/player";
import { getLastfmApiKey, setLastfmApiKey } from "../../lib/tauri/integration";
import { useLibrary } from "../../hooks/useLibrary";

interface SettingsViewProps {
  onNavigate: (view: ViewId) => void;
}

function ToggleSwitch({
  enabled,
  onToggle,
  label,
}: {
  enabled: boolean;
  onToggle: () => void;
  label: string;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      role="switch"
      aria-checked={enabled}
      aria-label={label}
      className={`relative w-12 h-7 rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 focus-visible:ring-offset-2 dark:focus-visible:ring-offset-zinc-900 ${
        enabled ? "bg-emerald-500" : "bg-zinc-300 dark:bg-zinc-600"
      }`}
    >
      <div
        className={`absolute top-0.5 w-6 h-6 rounded-full bg-white shadow-sm transition-transform ${
          enabled ? "left-[calc(100%-1.625rem)]" : "left-0.5"
        }`}
      />
    </button>
  );
}

interface LanguageDropdownProps {
  currentCode: string;
  onSelect: (code: string) => void;
}

function LanguageDropdown({ currentCode, onSelect }: LanguageDropdownProps) {
  const { t } = useTranslation();
  const [isOpen, setIsOpen] = useState(false);
  const [focusedIndex, setFocusedIndex] = useState(0);
  const containerRef = useRef<HTMLDivElement>(null);
  const optionRefs = useRef<(HTMLButtonElement | null)[]>([]);

  const currentLanguage =
    SUPPORTED_LANGUAGES.find((lang) => lang.code === currentCode) ??
    SUPPORTED_LANGUAGES[0];

  // Click-outside + Escape handling
  useEffect(() => {
    if (!isOpen) return;

    const handleClickOutside = (event: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(event.target as Node)
      ) {
        setIsOpen(false);
      }
    };

    const handleKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setIsOpen(false);
      }
    };

    document.addEventListener("mousedown", handleClickOutside);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
      document.removeEventListener("keydown", handleKey);
    };
  }, [isOpen]);

  // Keep keyboard focus on the highlighted option when it changes
  useEffect(() => {
    if (isOpen) {
      optionRefs.current[focusedIndex]?.focus();
    }
  }, [isOpen, focusedIndex]);

  const handleTriggerClick = () => {
    setIsOpen((prev) => {
      if (!prev) {
        // Opening: focus the currently selected option
        const initialIndex = Math.max(
          0,
          SUPPORTED_LANGUAGES.findIndex((lang) => lang.code === currentCode)
        );
        setFocusedIndex(initialIndex);
      }
      return !prev;
    });
  };

  const handleOptionKeyDown = (
    event: React.KeyboardEvent<HTMLButtonElement>,
    index: number
  ) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setFocusedIndex((index + 1) % SUPPORTED_LANGUAGES.length);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setFocusedIndex(
        (index - 1 + SUPPORTED_LANGUAGES.length) % SUPPORTED_LANGUAGES.length
      );
    } else if (event.key === "Home") {
      event.preventDefault();
      setFocusedIndex(0);
    } else if (event.key === "End") {
      event.preventDefault();
      setFocusedIndex(SUPPORTED_LANGUAGES.length - 1);
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      handleSelect(SUPPORTED_LANGUAGES[index].code);
    }
  };

  const handleSelect = (code: string) => {
    onSelect(code);
    setIsOpen(false);
  };

  return (
    <div className="relative" ref={containerRef}>
      <button
        type="button"
        onClick={handleTriggerClick}
        aria-haspopup="listbox"
        aria-expanded={isOpen}
        aria-label={t("settings.language.title")}
        className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
      >
        <span>{currentLanguage.nativeLabel}</span>
        <ChevronDown
          size={14}
          className={`transition-transform ${isOpen ? "rotate-180" : ""}`}
        />
      </button>

      {isOpen && (
        <ul
          role="listbox"
          aria-label={t("settings.language.title")}
          className="absolute top-full right-0 mt-2 min-w-48 rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated dark:shadow-black/40 overflow-hidden z-50 animate-fade-in py-1"
        >
          {SUPPORTED_LANGUAGES.map((lang, index) => {
            const isSelected = lang.code === currentCode;
            return (
              <li key={lang.code} role="presentation">
                <button
                  ref={(el) => {
                    optionRefs.current[index] = el;
                  }}
                  type="button"
                  role="option"
                  aria-selected={isSelected}
                  onClick={() => handleSelect(lang.code)}
                  onKeyDown={(event) => handleOptionKeyDown(event, index)}
                  className={`w-full flex items-center justify-between px-4 py-2 text-sm text-left transition-colors focus:outline-none ${
                    isSelected
                      ? "bg-emerald-50 text-emerald-700 dark:bg-emerald-900/20 dark:text-emerald-400"
                      : "text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700/30 focus:bg-zinc-50 dark:focus:bg-zinc-700/30"
                  }`}
                >
                  <span>{lang.nativeLabel}</span>
                  {isSelected && <Check size={14} />}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

export function SettingsView({ onNavigate }: SettingsViewProps) {
  const { t, i18n } = useTranslation();
  const { libraries, rescanLibrary } = useLibrary();
  const [isRescanning, setIsRescanning] = useState(false);
  const [autoStart, setAutoStart] = useState(false);
  const [minimizeToTray, setMinimizeToTray] = useState(true);
  const [scanOnStart, setScanOnStart] = useState(false);

  const handleRescan = async () => {
    if (isRescanning) return;
    setIsRescanning(true);
    try {
      for (const lib of libraries) {
        await rescanLibrary(lib.id);
      }
    } catch (err) {
      console.error("[SettingsView] rescan failed", err);
    } finally {
      setIsRescanning(false);
    }
  };

  // Audio settings — hydrated from backend at mount.
  const [normalize, setNormalize] = useState(false);
  const [mono, setMono] = useState(false);
  const [crossfadeSec, setCrossfadeSec] = useState(0);

  // Integrations
  const [lastfmKey, setLastfmKey] = useState("");
  const [lastfmKeyVisible, setLastfmKeyVisible] = useState(false);
  const [lastfmSaving, setLastfmSaving] = useState(false);
  const [lastfmSaved, setLastfmSaved] = useState(false);

  useEffect(() => {
    getLastfmApiKey()
      .then((v) => {
        if (v) setLastfmKey(v);
      })
      .catch(() => {});
  }, []);

  const handleSaveLastfmKey = async () => {
    if (lastfmSaving) return;
    setLastfmSaving(true);
    setLastfmSaved(false);
    try {
      await setLastfmApiKey(lastfmKey);
      setLastfmSaved(true);
      window.setTimeout(() => setLastfmSaved(false), 2000);
    } catch (err) {
      console.error("[SettingsView] save Last.fm key failed", err);
    } finally {
      setLastfmSaving(false);
    }
  };

  useEffect(() => {
    playerGetAudioSettings()
      .then((s) => {
        setNormalize(s.normalize);
        setMono(s.mono);
        setCrossfadeSec(Math.round(s.crossfade_ms / 1000));
      })
      .catch((err) => console.error("[Settings] audio settings load failed", err));
  }, []);

  const handleToggleNormalize = useCallback(() => {
    const next = !normalize;
    setNormalize(next);
    playerSetNormalize(next).catch((err) => {
      console.error("[Settings] set normalize failed", err);
      setNormalize(!next); // rollback
    });
  }, [normalize]);

  const handleToggleMono = useCallback(() => {
    const next = !mono;
    setMono(next);
    playerSetMono(next).catch((err) => {
      console.error("[Settings] set mono failed", err);
      setMono(!next);
    });
  }, [mono]);

  // Debounce crossfade slider changes to avoid spamming the backend.
  const crossfadeTimerRef = useRef<number | null>(null);
  const handleCrossfadeChange = useCallback((sec: number) => {
    setCrossfadeSec(sec);
    if (crossfadeTimerRef.current != null) {
      window.clearTimeout(crossfadeTimerRef.current);
    }
    crossfadeTimerRef.current = window.setTimeout(() => {
      playerSetCrossfade(sec).catch((err) =>
        console.error("[Settings] set crossfade failed", err)
      );
    }, 300);
  }, []);

  const handleLanguageChange = (code: string) => {
    i18n.changeLanguage(code);
  };

  return (
    <div className="max-w-4xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Header */}
      <div className="flex items-center space-x-4">
        <button
          type="button"
          onClick={() => onNavigate("home")}
          aria-label={t("common.back")}
          className="p-1 rounded-lg text-zinc-400 hover:text-zinc-800 dark:hover:text-white transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
        >
          <ArrowLeft size={20} />
        </button>
        <div>
          <h1 className="text-3xl font-bold text-zinc-900 dark:text-white">
            {t("settings.title")}
          </h1>
          <div className="w-8 h-1 bg-emerald-500 rounded-full mt-1" />
        </div>
      </div>

      {/* General Settings */}
      <section aria-labelledby="settings-general-heading">
        <h2
          id="settings-general-heading"
          className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
        >
          {t("settings.sections.general")}
        </h2>
        <div className="space-y-1">
          {/* Langue */}
          <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
            <div className="flex items-center space-x-4">
              <Globe
                size={20}
                className="text-zinc-400"
                aria-hidden="true"
              />
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.language.title")}
                </div>
                <div className="text-xs text-zinc-400">
                  {t("settings.language.subtitle")}
                </div>
              </div>
            </div>
            <LanguageDropdown
              currentCode={i18n.resolvedLanguage ?? "fr"}
              onSelect={handleLanguageChange}
            />
          </div>

          {/* Lancement au démarrage */}
          <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
            <div className="flex items-center space-x-4">
              <Power
                size={20}
                className="text-zinc-400"
                aria-hidden="true"
              />
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.autoStart.title")}
                </div>
                <div className="text-xs text-zinc-400">
                  {t("settings.autoStart.subtitle")}
                </div>
              </div>
            </div>
            <ToggleSwitch
              enabled={autoStart}
              onToggle={() => setAutoStart(!autoStart)}
              label={t("settings.autoStart.title")}
            />
          </div>

          {/* Minimiser dans la barre système */}
          <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
            <div className="flex items-center space-x-4">
              <Minimize2
                size={20}
                className="text-zinc-400"
                aria-hidden="true"
              />
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.minimizeToTray.title")}
                </div>
                <div className="text-xs text-zinc-400">
                  {t("settings.minimizeToTray.subtitle")}
                </div>
              </div>
            </div>
            <ToggleSwitch
              enabled={minimizeToTray}
              onToggle={() => setMinimizeToTray(!minimizeToTray)}
              label={t("settings.minimizeToTray.title")}
            />
          </div>

          {/* Scanner au démarrage */}
          <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
            <div className="flex items-center space-x-4">
              <ScanLine
                size={20}
                className="text-zinc-400"
                aria-hidden="true"
              />
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.scanOnStart.title")}
                </div>
                <div className="text-xs text-zinc-400">
                  {t("settings.scanOnStart.subtitle")}
                </div>
              </div>
            </div>
            <ToggleSwitch
              enabled={scanOnStart}
              onToggle={() => setScanOnStart(!scanOnStart)}
              label={t("settings.scanOnStart.title")}
            />
          </div>
        </div>
      </section>

      {/* Lecture (Audio) */}
      <section aria-labelledby="settings-playback-heading">
        <h2
          id="settings-playback-heading"
          className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
        >
          {t("settings.sections.playback")}
        </h2>
        <div className="space-y-1">
          {/* Crossfade */}
          <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
            <div className="flex items-center space-x-4">
              <Shuffle
                size={20}
                className="text-zinc-400"
                aria-hidden="true"
              />
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.crossfade.title")}
                </div>
                <div className="text-xs text-zinc-400">
                  {t("settings.crossfade.subtitle")}
                </div>
              </div>
            </div>
            <div className="flex items-center space-x-3">
              <input
                type="range"
                min={0}
                max={12}
                step={1}
                value={crossfadeSec}
                onChange={(e) =>
                  handleCrossfadeChange(Number(e.target.value))
                }
                className="w-32 h-1.5 rounded-full appearance-none bg-zinc-200 dark:bg-zinc-700 accent-emerald-500 cursor-pointer"
                aria-label={t("settings.crossfade.title")}
              />
              <span className="text-sm font-medium text-zinc-500 w-10 text-right tabular-nums">
                {crossfadeSec} s
              </span>
            </div>
          </div>

          {/* Normaliser le volume */}
          <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
            <div className="flex items-center space-x-4">
              <Volume2
                size={20}
                className="text-zinc-400"
                aria-hidden="true"
              />
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.normalize.title")}
                </div>
                <div className="text-xs text-zinc-400">
                  {t("settings.normalize.subtitle")}
                </div>
              </div>
            </div>
            <ToggleSwitch
              enabled={normalize}
              onToggle={handleToggleNormalize}
              label={t("settings.normalize.title")}
            />
          </div>

          {/* Audio mono */}
          <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
            <div className="flex items-center space-x-4">
              <Headphones
                size={20}
                className="text-zinc-400"
                aria-hidden="true"
              />
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.mono.title")}
                </div>
                <div className="text-xs text-zinc-400">
                  {t("settings.mono.subtitle")}
                </div>
              </div>
            </div>
            <ToggleSwitch
              enabled={mono}
              onToggle={handleToggleMono}
              label={t("settings.mono.title")}
            />
          </div>
        </div>
      </section>

      {/* Intégrations */}
      <section aria-labelledby="settings-integrations-heading">
        <h2
          id="settings-integrations-heading"
          className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
        >
          {t("settings.sections.integrations")}
        </h2>
        <div className="space-y-1">
          <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
            <div className="flex items-start space-x-4">
              <Radio
                size={20}
                className="text-zinc-400 mt-0.5"
                aria-hidden="true"
              />
              <div className="flex-1 min-w-0">
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.integrations.lastfm.title")}
                </div>
                <div className="text-xs text-zinc-400 mb-3">
                  {t("settings.integrations.lastfm.subtitle")}
                </div>
                <div className="flex items-center space-x-2">
                  <div className="relative flex-1">
                    <input
                      type={lastfmKeyVisible ? "text" : "password"}
                      value={lastfmKey}
                      onChange={(e) => {
                        setLastfmKey(e.target.value);
                        setLastfmSaved(false);
                      }}
                      placeholder={t("settings.integrations.lastfm.placeholder")}
                      spellCheck={false}
                      autoComplete="off"
                      className="w-full pr-10 pl-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500"
                    />
                    <button
                      type="button"
                      onClick={() => setLastfmKeyVisible((v) => !v)}
                      aria-label={
                        lastfmKeyVisible
                          ? t("settings.integrations.lastfm.hide")
                          : t("settings.integrations.lastfm.show")
                      }
                      className="absolute inset-y-0 right-0 px-3 flex items-center text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
                    >
                      {lastfmKeyVisible ? <EyeOff size={16} /> : <Eye size={16} />}
                    </button>
                  </div>
                  <button
                    type="button"
                    onClick={handleSaveLastfmKey}
                    disabled={lastfmSaving}
                    className={`px-4 py-2 rounded-xl text-sm font-medium transition-colors disabled:opacity-50 ${
                      lastfmSaved
                        ? "bg-emerald-500 text-white"
                        : "border border-zinc-200 bg-white text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700"
                    }`}
                  >
                    {lastfmSaved
                      ? t("settings.integrations.lastfm.saved")
                      : t("settings.integrations.lastfm.save")}
                  </button>
                </div>
              </div>
            </div>
          </div>
        </div>
      </section>

      {/* Stockage & Données */}
      <section aria-labelledby="settings-storage-heading">
        <h2
          id="settings-storage-heading"
          className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
        >
          {t("settings.sections.storage")}
        </h2>
        <div className="space-y-1">
          <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
            <div className="flex items-center space-x-4">
              <RefreshCcw
                size={20}
                className="text-zinc-400"
                aria-hidden="true"
              />
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.rescan.title")}
                </div>
                <div className="text-xs text-zinc-400">
                  {t("settings.rescan.subtitle")}
                </div>
              </div>
            </div>
            <button
              type="button"
              onClick={handleRescan}
              disabled={isRescanning || libraries.length === 0}
              className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <RefreshCcw
                size={14}
                aria-hidden="true"
                className={isRescanning ? "animate-spin" : ""}
              />
              <span>{t("settings.rescan.action")}</span>
            </button>
          </div>

          <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
            <div className="flex items-center space-x-4">
              <ImageIcon
                size={20}
                className="text-zinc-400"
                aria-hidden="true"
              />
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.artistImages.title")}
                </div>
                <div className="text-xs text-zinc-400">
                  {t("settings.artistImages.subtitle")}
                </div>
              </div>
            </div>
            <button
              type="button"
              className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
            >
              <ImageIcon size={14} aria-hidden="true" />
              <span>{t("settings.artistImages.action")}</span>
            </button>
          </div>

          <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
            <div className="flex items-center space-x-4">
              <FolderOpen
                size={20}
                className="text-zinc-400"
                aria-hidden="true"
              />
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.dataFolder.title")}
                </div>
                <div className="text-xs text-zinc-400">
                  {t("settings.dataFolder.subtitle")}
                </div>
              </div>
            </div>
            <button
              type="button"
              className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
            >
              <FolderOpen size={14} aria-hidden="true" />
              <span>{t("settings.dataFolder.action")}</span>
            </button>
          </div>

          <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
            <div className="flex items-center space-x-4">
              <Trash2
                size={20}
                className="text-zinc-400"
                aria-hidden="true"
              />
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-white">
                  {t("settings.reset.title")}
                </div>
                <div className="text-xs text-zinc-400">
                  {t("settings.reset.subtitle")}
                </div>
              </div>
            </div>
            <button
              type="button"
              className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-red-200 bg-white text-sm font-medium text-red-500 hover:bg-red-50 dark:border-red-500/30 dark:bg-zinc-800 dark:hover:bg-red-500/10 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-red-500"
            >
              <Trash2 size={14} aria-hidden="true" />
              <span>{t("settings.reset.action")}</span>
            </button>
          </div>
        </div>
      </section>
    </div>
  );
}

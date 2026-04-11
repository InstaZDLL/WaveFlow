import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Home,
  File,
  Folder,
  Library,
  Music2,
  Disc,
  Mic2,
  Tags,
  Heart,
  Clock,
  Plus,
  Check,
  ChevronDown,
  ChevronUp,
  ArrowRight,
} from "lucide-react";
import type { ViewId, LibraryTab } from "../../types";
import { NavItem } from "../common/NavItem";
import { WaveFlowLogo } from "../common/WaveFlowLogo";
import { CreateLibraryModal } from "../common/CreateLibraryModal";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { useProfile } from "../../hooks/useProfile";
import { useLibrary } from "../../hooks/useLibrary";
import { getProfileColor, profileInitial } from "../../lib/profileColors";
import { pickFolder } from "../../lib/tauri/dialog";

interface SidebarProps {
  activeView: ViewId;
  setActiveView: (view: ViewId) => void;
  libraryTab: LibraryTab;
  setLibraryTab: (tab: LibraryTab) => void;
}

export function Sidebar({ activeView, setActiveView, libraryTab, setLibraryTab }: SidebarProps) {
  const { t } = useTranslation();
  const { activeProfile } = useProfile();
  const {
    libraries,
    selectedLibraryId,
    selectedLibrary,
    selectLibrary,
    createLibrary,
    importFolder,
  } = useLibrary();
  const profileColor = getProfileColor(activeProfile?.color_id);
  const [isLibraryPopoverOpen, setIsLibraryPopoverOpen] = useState(false);
  const [isCreateLibraryModalOpen, setIsCreateLibraryModalOpen] =
    useState(false);
  const [isCreatePlaylistModalOpen, setIsCreatePlaylistModalOpen] =
    useState(false);
  const [isImporting, setIsImporting] = useState(false);
  const libraryPopoverRef = useRef<HTMLDivElement>(null);

  const handleCreateLibrary = async (name: string, description: string) => {
    try {
      await createLibrary({
        name,
        description: description || null,
      });
    } catch (err) {
      console.error("[Sidebar] failed to create library", err);
    }
  };

  const handleImportFolder = async () => {
    if (isImporting) return;
    const libraryId = selectedLibraryId;
    if (libraryId == null) {
      setIsCreateLibraryModalOpen(true);
      return;
    }
    try {
      const path = await pickFolder(t("sidebar.open.folder"));
      if (!path) return;
      setIsImporting(true);
      await importFolder(libraryId, path);
    } catch (err) {
      console.error("[Sidebar] import failed", err);
    } finally {
      setIsImporting(false);
    }
  };

  useEffect(() => {
    if (!isLibraryPopoverOpen) return;

    const handleClickOutside = (event: MouseEvent) => {
      if (
        libraryPopoverRef.current &&
        !libraryPopoverRef.current.contains(event.target as Node)
      ) {
        setIsLibraryPopoverOpen(false);
      }
    };

    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setIsLibraryPopoverOpen(false);
    };

    document.addEventListener("mousedown", handleClickOutside);
    document.addEventListener("keydown", handleEscape);
    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
      document.removeEventListener("keydown", handleEscape);
    };
  }, [isLibraryPopoverOpen]);

  return (
    <div className="w-64 flex flex-col border-r h-full border-zinc-200 bg-white dark:border-zinc-800 dark:bg-surface-dark">
      {/* Brand & Profile */}
      <div className="p-6 pb-2">
        <div className="flex items-center space-x-2 font-bold text-xl mb-6">
          <WaveFlowLogo className="w-7 h-7" />
          <span className="text-zinc-800 dark:text-white">WaveFlow</span>
          <span className="bg-emerald-500 text-white text-[9px] px-1.5 py-0.5 rounded uppercase tracking-wider">
            {t("sidebar.brand.beta")}
          </span>
        </div>

        <button className="w-full flex items-center justify-between p-2 rounded-xl border transition-colors border-zinc-200 bg-zinc-50 hover:bg-zinc-100 text-zinc-700 dark:border-zinc-700 dark:bg-zinc-800/50 dark:hover:bg-zinc-800 dark:text-zinc-200">
          <div className="flex items-center space-x-3 min-w-0">
            <div
              className={`w-6 h-6 rounded-full ${profileColor.avatarBg} ${profileColor.avatarText} flex items-center justify-center text-xs font-bold shrink-0`}
            >
              {activeProfile ? profileInitial(activeProfile.name) : ""}
            </div>
            <span className="text-sm font-medium truncate">
              {activeProfile?.name ?? ""}
            </span>
          </div>
          <div className={`w-2 h-2 rounded-full ${profileColor.dot} shrink-0`} />
        </button>
      </div>

      <div className="flex-1 overflow-y-auto p-4 space-y-6 scrollbar-hide">
        {/* Navigation */}
        <div className="space-y-1">
          <NavItem
            icon={<Home size={18} />}
            label={t("sidebar.nav.home")}
            active={activeView === "home"}
            onClick={() => setActiveView("home")}
          />
        </div>

        {/* Ouvrir */}
        <div>
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 mb-3 px-2 uppercase">
            {t("sidebar.sections.open")}
          </div>
          <div className="flex space-x-2 px-2">
            <button className="flex-1 flex items-center justify-center space-x-2 py-2 rounded-xl border text-xs font-medium transition-colors border-zinc-200 hover:bg-zinc-50 text-zinc-600 dark:border-zinc-700 dark:hover:bg-zinc-800 dark:text-zinc-300">
              <File size={14} /> <span>{t("sidebar.open.file")}</span>
            </button>
            <button
              type="button"
              onClick={handleImportFolder}
              disabled={isImporting}
              className="flex-1 flex items-center justify-center space-x-2 py-2 rounded-xl border text-xs font-medium transition-colors border-zinc-200 hover:bg-zinc-50 text-zinc-600 dark:border-zinc-700 dark:hover:bg-zinc-800 dark:text-zinc-300 disabled:opacity-60 disabled:cursor-wait"
            >
              <Folder size={14} /> <span>{t("sidebar.open.folder")}</span>
            </button>
          </div>
        </div>

        {/* Bibliothèque */}
        <div>
          <div className="flex items-center justify-between text-[10px] font-bold tracking-widest text-zinc-400 mb-2 px-2 uppercase">
            <span>{t("sidebar.sections.library")}</span>
            <button
              type="button"
              onClick={() => setIsCreateLibraryModalOpen(true)}
              aria-label={t("sidebar.library.createLibraryAria")}
              className="p-0.5 rounded hover:text-emerald-500 transition-colors"
            >
              <Plus size={14} />
            </button>
          </div>

          <div className="space-y-1">
            {/* Library selector with popover */}
            <div className="relative" ref={libraryPopoverRef}>
              <button
                type="button"
                onClick={() => {
                  if (libraries.length === 0) {
                    setIsCreateLibraryModalOpen(true);
                    return;
                  }
                  setIsLibraryPopoverOpen((prev) => !prev);
                }}
                aria-haspopup="listbox"
                aria-expanded={isLibraryPopoverOpen}
                className={`w-full flex items-center justify-between p-2 rounded-xl border transition-colors ${
                  isLibraryPopoverOpen
                    ? "border-zinc-200 bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800/50"
                    : "border-transparent hover:bg-zinc-100 dark:hover:bg-zinc-800/50"
                }`}
              >
                <div className="flex items-center space-x-3 min-w-0">
                  <div className="w-10 h-10 rounded-lg bg-emerald-100 text-emerald-600 dark:bg-emerald-950/60 dark:text-emerald-400 flex items-center justify-center shrink-0">
                    <Library size={20} />
                  </div>
                  <div className="text-left min-w-0">
                    <div className="text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
                      {selectedLibrary?.name ?? t("sidebar.library.none")}
                    </div>
                    <div className="text-xs text-zinc-500">
                      {t("sidebar.library.tracksCount", {
                        count: selectedLibrary?.track_count ?? 0,
                      })}
                    </div>
                  </div>
                </div>
                {libraries.length > 0 &&
                  (isLibraryPopoverOpen ? (
                    <ChevronUp size={16} className="text-zinc-400 shrink-0" />
                  ) : (
                    <ChevronDown size={16} className="text-zinc-400 shrink-0" />
                  ))}
              </button>

              {isLibraryPopoverOpen && (
                <div
                  role="listbox"
                  aria-label={t("sidebar.library.popover.label")}
                  className="absolute top-full left-0 right-0 mt-2 z-50 rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated dark:shadow-black/40 overflow-hidden animate-fade-in"
                >
                  <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase px-3 pt-3 pb-2">
                    {t("sidebar.library.popover.header")}
                  </div>

                  <div className="px-2 pb-2 space-y-1 max-h-64 overflow-y-auto">
                    {libraries.map((lib) => {
                      const isSelected = lib.id === selectedLibraryId;
                      return (
                        <button
                          key={lib.id}
                          type="button"
                          role="option"
                          aria-selected={isSelected}
                          onClick={() => {
                            selectLibrary(lib.id);
                            setIsLibraryPopoverOpen(false);
                          }}
                          className={`w-full flex items-center space-x-2 p-2 rounded-lg text-left transition-colors ${
                            isSelected
                              ? "bg-emerald-50 dark:bg-emerald-900/20"
                              : "hover:bg-zinc-50 dark:hover:bg-zinc-700/30"
                          }`}
                        >
                          <div
                            className={`w-8 h-8 rounded-lg flex items-center justify-center shrink-0 ${
                              isSelected
                                ? "bg-emerald-500 text-white"
                                : "bg-emerald-100 text-emerald-600 dark:bg-emerald-950/60 dark:text-emerald-400"
                            }`}
                          >
                            {isSelected ? (
                              <Check size={14} />
                            ) : (
                              <Library size={16} />
                            )}
                          </div>
                          <div className="flex-1 min-w-0">
                            <div
                              className={`text-sm font-medium truncate ${
                                isSelected
                                  ? "text-emerald-700 dark:text-emerald-400"
                                  : "text-zinc-800 dark:text-zinc-200"
                              }`}
                            >
                              {lib.name}
                            </div>
                            <div className="text-[11px] text-zinc-500">
                              {t("sidebar.library.popover.countLine", {
                                tracks: t("sidebar.library.popover.tracks", {
                                  count: lib.track_count,
                                }),
                                albums: t("sidebar.library.popover.albums", {
                                  count: lib.album_count,
                                }),
                              })}
                            </div>
                          </div>
                        </button>
                      );
                    })}
                  </div>

                  <div className="border-t border-zinc-100 dark:border-zinc-700/50 flex items-center justify-between px-4 py-2 text-xs font-medium">
                    <button
                      type="button"
                      onClick={() => {
                        setActiveView("library");
                        setIsLibraryPopoverOpen(false);
                      }}
                      className="text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-300 flex items-center space-x-1"
                    >
                      <ArrowRight size={12} />
                      <span>{t("sidebar.library.popover.see")}</span>
                    </button>
                    <button
                      type="button"
                      onClick={() => {
                        setIsLibraryPopoverOpen(false);
                        setIsCreateLibraryModalOpen(true);
                      }}
                      className="text-emerald-500 hover:text-emerald-600 flex items-center space-x-1"
                    >
                      <Plus size={12} />
                      <span>{t("sidebar.library.popover.new")}</span>
                    </button>
                  </div>
                </div>
              )}
            </div>

            <NavItem
              icon={<Music2 size={18} />}
              label={t("sidebar.library.nav.tracks")}
              active={activeView === "library" && libraryTab === "morceaux"}
              onClick={() => { setLibraryTab("morceaux"); setActiveView("library"); }}
            />
            <NavItem
              icon={<Disc size={18} />}
              label={t("sidebar.library.nav.albums")}
              active={activeView === "library" && libraryTab === "albums"}
              onClick={() => { setLibraryTab("albums"); setActiveView("library"); }}
            />
            <NavItem
              icon={<Mic2 size={18} />}
              label={t("sidebar.library.nav.artists")}
              active={activeView === "library" && libraryTab === "artistes"}
              onClick={() => { setLibraryTab("artistes"); setActiveView("library"); }}
            />
            <NavItem
              icon={<Tags size={18} />}
              label={t("sidebar.library.nav.genres")}
              active={activeView === "library" && libraryTab === "genres"}
              onClick={() => { setLibraryTab("genres"); setActiveView("library"); }}
            />
            <NavItem
              icon={<Folder size={18} />}
              label={t("sidebar.library.nav.explorer")}
              active={activeView === "library" && libraryTab === "dossiers"}
              onClick={() => { setLibraryTab("dossiers"); setActiveView("library"); }}
            />
          </div>
        </div>

        {/* Separator */}
        <div className="border-t border-zinc-200 dark:border-zinc-700/50 mx-2" />

        {/* Playlists */}
        <div className="bg-zinc-50/50 dark:bg-zinc-800/20 rounded-xl p-2 -mx-0.5">
          <div className="flex items-center justify-between text-[10px] font-bold tracking-widest text-zinc-400 mb-3 px-2 uppercase">
            <span>{t("sidebar.sections.playlists")}</span>
            <button
              type="button"
              onClick={() => setIsCreatePlaylistModalOpen(true)}
              aria-label={t("sidebar.playlists.createPlaylistAria")}
              className="p-0.5 rounded hover:text-emerald-500 transition-colors"
            >
              <Plus size={14} />
            </button>
          </div>
          <div className="space-y-1">
            <NavItem
              customIcon={
                <div className="w-8 h-8 rounded-lg bg-pink-100 text-pink-500 flex items-center justify-center dark:bg-pink-950/60 dark:text-pink-400">
                  <Heart size={16} className="fill-current" />
                </div>
              }
              label={t("sidebar.playlists.liked")}
              subtext={t("sidebar.playlists.emptySubtext", { count: 0 })}
              active={activeView === "liked"}
              onClick={() => setActiveView("liked")}
            />
            <NavItem
              customIcon={
                <div className="w-8 h-8 rounded-lg bg-blue-100 text-blue-500 flex items-center justify-center dark:bg-blue-950/60 dark:text-blue-400">
                  <Clock size={16} />
                </div>
              }
              label={t("sidebar.playlists.recent")}
              subtext={t("sidebar.playlists.emptySubtext", { count: 0 })}
              active={activeView === "recent"}
              onClick={() => setActiveView("recent")}
            />
          </div>
        </div>
      </div>

      <CreateLibraryModal
        isOpen={isCreateLibraryModalOpen}
        onClose={() => setIsCreateLibraryModalOpen(false)}
        onCreate={handleCreateLibrary}
      />

      <CreatePlaylistModal
        isOpen={isCreatePlaylistModalOpen}
        onClose={() => setIsCreatePlaylistModalOpen(false)}
      />
    </div>
  );
}

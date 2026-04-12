import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  Home,
  Folder,
  Music2,
  Disc,
  Mic2,
  Tags,
  Heart,
  Clock,
  Plus,
} from "lucide-react";
import type { ViewId, LibraryTab } from "../../types";
import { NavItem } from "../common/NavItem";
import { WaveFlowLogo } from "../common/WaveFlowLogo";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { useProfile } from "../../hooks/useProfile";
import { useLibrary } from "../../hooks/useLibrary";
import { usePlaylist } from "../../hooks/usePlaylist";
import { getProfileColor, profileInitial } from "../../lib/profileColors";
import { pickFolder } from "../../lib/tauri/dialog";
import { getProfileStats, type ProfileStats } from "../../lib/tauri/browse";
import { resolvePlaylistColor } from "../../lib/playlistVisuals";
import { PlaylistIcon } from "../../lib/PlaylistIcon";
import type { Playlist } from "../../lib/tauri/playlist";

interface SidebarProps {
  activeView: ViewId;
  setActiveView: (view: ViewId) => void;
  libraryTab: LibraryTab;
  setLibraryTab: (tab: LibraryTab) => void;
  activePlaylistId: number | null;
  setActivePlaylistId: (id: number | null) => void;
}

export function Sidebar({
  activeView,
  setActiveView,
  libraryTab,
  setLibraryTab,
  activePlaylistId,
  setActivePlaylistId,
}: SidebarProps) {
  const { t } = useTranslation();
  const { activeProfile } = useProfile();
  const {
    libraries,
    selectedLibraryId,
    selectLibrary,
    createLibrary,
    importFolder,
  } = useLibrary();
  const { playlists, createPlaylist } = usePlaylist();
  const profileColor = getProfileColor(activeProfile?.color_id);
  const [isCreatePlaylistModalOpen, setIsCreatePlaylistModalOpen] =
    useState(false);
  const [isImporting, setIsImporting] = useState(false);
  const [stats, setStats] = useState<ProfileStats>({
    liked_count: 0,
    recent_plays_count: 0,
  });

  // Seq-guarded stats fetch
  const statsSeqRef = useRef(0);
  const refreshStats = useCallback(() => {
    const seq = ++statsSeqRef.current;
    getProfileStats()
      .then((s) => {
        if (seq === statsSeqRef.current) setStats(s);
      })
      .catch((err) => console.error("[Sidebar] profile stats failed", err));
  }, []);

  useEffect(() => {
    refreshStats();
    let unlisten: UnlistenFn | null = null;
    (async () => {
      try {
        unlisten = await listen("player:track-changed", () => refreshStats());
      } catch (err) {
        console.error("[Sidebar] listen failed", err);
      }
    })();
    return () => {
      if (unlisten) unlisten();
    };
    // Re-fetch when the active profile changes so counters reflect
    // the new profile's data immediately (not just after a track swap).
  }, [refreshStats, activeProfile]);

  // Import a folder: auto-create a library if none exists, then scan.
  const handleImportFolder = async () => {
    if (isImporting) return;
    try {
      const path = await pickFolder(t("sidebar.open.folder"));
      if (!path) return;
      setIsImporting(true);

      let libraryId = selectedLibraryId;
      // Auto-create a default library if the profile has none.
      if (libraryId == null) {
        if (libraries.length > 0) {
          libraryId = libraries[0].id;
          selectLibrary(libraryId);
        } else {
          const lib = await createLibrary({ name: "Ma musique" });
          libraryId = lib.id;
          selectLibrary(libraryId);
        }
      }
      await importFolder(libraryId, path);
    } catch (err) {
      console.error("[Sidebar] import failed", err);
    } finally {
      setIsImporting(false);
    }
  };

  const handleCreatePlaylistSubmit = async (data: {
    name: string;
    description: string;
    colorId: string;
    iconId: string;
  }) => {
    try {
      const created = await createPlaylist({
        name: data.name,
        description: data.description || null,
        color_id: data.colorId,
        icon_id: data.iconId,
      });
      setActivePlaylistId(created.id);
      setActiveView("playlist");
    } catch (err) {
      console.error("[Sidebar] failed to create playlist", err);
    }
  };

  const handleSelectPlaylist = (playlistId: number) => {
    setActivePlaylistId(playlistId);
    setActiveView("playlist");
  };

  const isPlaylistRowActive = (id: number) =>
    activeView === "playlist" && activePlaylistId === id;

  // Memoized pinned rows (liked + recent)
  const pinnedRows = useMemo(
    () => [
      {
        key: "liked",
        label: t("sidebar.playlists.liked"),
        subtext: t("sidebar.playlists.emptySubtext", {
          count: stats.liked_count,
        }),
        active: activeView === "liked",
        onClick: () => setActiveView("liked"),
        icon: (
          <div className="w-8 h-8 rounded-lg bg-pink-100 text-pink-500 flex items-center justify-center dark:bg-pink-950/60 dark:text-pink-400">
            <Heart size={16} className="fill-current" />
          </div>
        ),
      },
      {
        key: "recent",
        label: t("sidebar.playlists.recent"),
        subtext: t("sidebar.playlists.emptySubtext", {
          count: stats.recent_plays_count,
        }),
        active: activeView === "recent",
        onClick: () => setActiveView("recent"),
        icon: (
          <div className="w-8 h-8 rounded-lg bg-blue-100 text-blue-500 flex items-center justify-center dark:bg-blue-950/60 dark:text-blue-400">
            <Clock size={16} />
          </div>
        ),
      },
    ],
    [
      t,
      stats.liked_count,
      stats.recent_plays_count,
      activeView,
      setActiveView,
    ]
  );

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

      <div className="flex-1 flex flex-col p-4 space-y-4 overflow-hidden">
        {/* Navigation */}
        <div className="space-y-1 shrink-0">
          <NavItem
            icon={<Home size={18} />}
            label={t("sidebar.nav.home")}
            active={activeView === "home"}
            onClick={() => setActiveView("home")}
          />
        </div>

        {/* ─── MA MUSIQUE ─── */}
        <div className="space-y-1 shrink-0">
          <div className="flex items-center justify-between text-[10px] font-bold tracking-widest text-zinc-400 mb-1 px-2 uppercase">
            <span>{t("sidebar.myMusic.title")}</span>
            <button
              type="button"
              onClick={handleImportFolder}
              disabled={isImporting}
              aria-label={t("sidebar.myMusic.addFolderAria")}
              className="p-0.5 rounded hover:text-emerald-500 transition-colors disabled:opacity-50"
            >
              <Folder size={14} />
            </button>
          </div>
          <NavItem
            icon={<Music2 size={18} />}
            label={t("sidebar.myMusic.tracks")}
            active={activeView === "library" && libraryTab === "morceaux"}
            onClick={() => { setLibraryTab("morceaux"); setActiveView("library"); }}
          />
          <NavItem
            icon={<Disc size={18} />}
            label={t("sidebar.myMusic.albums")}
            active={activeView === "library" && libraryTab === "albums"}
            onClick={() => { setLibraryTab("albums"); setActiveView("library"); }}
          />
          <NavItem
            icon={<Mic2 size={18} />}
            label={t("sidebar.myMusic.artists")}
            active={activeView === "library" && libraryTab === "artistes"}
            onClick={() => { setLibraryTab("artistes"); setActiveView("library"); }}
          />
          <NavItem
            icon={<Tags size={18} />}
            label={t("sidebar.myMusic.genres")}
            active={activeView === "library" && libraryTab === "genres"}
            onClick={() => { setLibraryTab("genres"); setActiveView("library"); }}
          />
          <NavItem
            icon={<Folder size={18} />}
            label={t("sidebar.myMusic.folders")}
            active={activeView === "library" && libraryTab === "dossiers"}
            onClick={() => { setLibraryTab("dossiers"); setActiveView("library"); }}
          />
        </div>

        {/* ─── PLAYLISTS ─── */}
        <div className="flex-1 flex flex-col min-h-0">
          <div className="flex items-center justify-between text-[10px] font-bold tracking-widest text-zinc-400 mb-1 px-2 uppercase shrink-0">
            <span>{t("sidebar.playlistSection.title")}</span>
            <button
              type="button"
              onClick={() => setIsCreatePlaylistModalOpen(true)}
              aria-label={t("sidebar.playlists.createPlaylistAria")}
              className="p-0.5 rounded hover:text-emerald-500 transition-colors"
            >
              <Plus size={14} />
            </button>
          </div>

          <div className="flex-1 min-h-0 overflow-y-auto space-y-1 scrollbar-hide pr-1">
            {/* Pinned: Liked + Recent */}
            {pinnedRows.map((row) => (
              <SidebarRow
                key={row.key}
                icon={row.icon}
                label={row.label}
                subtext={row.subtext}
                active={row.active}
                onClick={row.onClick}
              />
            ))}

            {/* Real playlists */}
            {playlists.map((pl) => (
              <PlaylistSidebarRow
                key={`pl-${pl.id}`}
                playlist={pl}
                active={isPlaylistRowActive(pl.id)}
                onClick={() => handleSelectPlaylist(pl.id)}
                subtext={t("sidebar.myLibrary.playlistSubtext", {
                  count: pl.track_count,
                })}
              />
            ))}
          </div>
        </div>
      </div>

      <CreatePlaylistModal
        isOpen={isCreatePlaylistModalOpen}
        onClose={() => setIsCreatePlaylistModalOpen(false)}
        onCreate={handleCreatePlaylistSubmit}
      />
    </div>
  );
}

function SidebarRow({
  icon,
  label,
  subtext,
  active,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  subtext?: string;
  active?: boolean;
  onClick?: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`w-full flex items-center space-x-3 p-2 rounded-xl text-left transition-colors ${
        active
          ? "bg-emerald-50 dark:bg-emerald-900/20"
          : "hover:bg-zinc-50 dark:hover:bg-zinc-800/40"
      }`}
    >
      <div className="shrink-0">{icon}</div>
      <div className="flex-1 min-w-0">
        <div
          className={`text-sm font-medium truncate ${
            active
              ? "text-emerald-700 dark:text-emerald-400"
              : "text-zinc-800 dark:text-zinc-200"
          }`}
        >
          {label}
        </div>
        {subtext && (
          <div className="text-[11px] text-zinc-500 truncate">{subtext}</div>
        )}
      </div>
    </button>
  );
}

function PlaylistSidebarRow({
  playlist,
  active,
  onClick,
  subtext,
}: {
  playlist: Playlist;
  active?: boolean;
  onClick?: () => void;
  subtext: string;
}) {
  const color = resolvePlaylistColor(playlist.color_id);
  return (
    <SidebarRow
      icon={
        <div
          className={`w-8 h-8 rounded-lg flex items-center justify-center ${color.tileBg} ${color.tileText}`}
        >
          <PlaylistIcon iconId={playlist.icon_id} size={16} />
        </div>
      }
      label={playlist.name}
      subtext={subtext}
      active={active}
      onClick={onClick}
    />
  );
}
